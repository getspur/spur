//! B-tree cursor implementation (§11, bd-2kvo).
//!
//! A cursor maintains a position within a single B-tree (either table or
//! index) and supports seek, navigation, and payload access. The cursor
//! uses a page stack of depth up to [`BTREE_MAX_DEPTH`] (20) to track
//! its path from root to leaf.
//!
//! # Architecture
//!
//! ```text
//!               ┌─────────┐
//!   stack[0]    │ Root     │  cell_idx = 1
//!               └────┬────┘
//!               ┌────▼────┐
//!   stack[1]    │ Interior│  cell_idx = 0
//!               └────┬────┘
//!               ┌────▼────┐
//!   stack[2]    │ Leaf    │  cell_idx = 3  ← current position
//!               └─────────┘
//! ```

use crate::balance;
use crate::cell::{self, BtreePageHeader, CellRef};
use crate::instrumentation::{self, BtreeOpRuntimeStats, BtreeOpType};
use crate::overflow;
use crate::traits::{BtreeCursorOps, SeekResult, sealed};
use fsqlite_error::{FrankenError, Result};
use fsqlite_pager::TransactionHandle;
use fsqlite_types::cx::Cx;
use fsqlite_types::limits::BTREE_MAX_DEPTH;
use fsqlite_types::record::{RecordProfileScope, enter_record_profile_scope, parse_record};
use fsqlite_types::serial_type::{
    SerialTypeClass, classify_serial_type, read_varint, serial_type_len, write_varint,
};
use fsqlite_types::{PageData, PageNumber, WitnessKey};
use std::borrow::Cow;
use std::sync::Arc;
use tracing::{Level, debug, trace, warn};

#[inline]
fn observe_cursor_cancellation(cx: &Cx) -> Result<()> {
    cx.checkpoint().map_err(|_| FrankenError::Abort)
}

// ---------------------------------------------------------------------------
// Page reader trait (for testability)
// ---------------------------------------------------------------------------

/// Trait for reading raw page data by page number.
///
/// This allows the cursor to be tested with an in-memory page store.
/// The real implementation wraps a `TransactionHandle`.
pub trait PageReader {
    /// Read a page by number, returning the raw bytes.
    fn read_page(&self, cx: &Cx, page_no: PageNumber) -> Result<Vec<u8>>;

    /// Read a page by number, returning owned page data.
    ///
    /// Implementations can override this to forward shared page buffers
    /// without forcing an intermediate `Vec<u8>` clone.
    fn read_page_data(&self, cx: &Cx, page_no: PageNumber) -> Result<PageData> {
        Ok(PageData::from_vec(self.read_page(cx, page_no)?))
    }

    /// Hint that a page is likely to be needed soon.
    ///
    /// Default implementation is a no-op so platforms without a safe prefetch
    /// primitive degrade gracefully.
    fn prefetch_page_hint(&self, _cx: &Cx, _page_no: PageNumber) {}

    /// Record a granular read witness for fine-grained SSI validation.
    fn record_read_witness(&self, _cx: &Cx, _key: WitnessKey) {}

    /// Returns `true` if the page has been modified in the current transaction.
    fn is_dirty(&self, _page_no: PageNumber) -> bool {
        false
    }
}

/// Trait for writing pages (needed for insert/delete).
pub trait PageWriter: PageReader {
    /// Write raw data to a page.
    fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()>;

    /// Write owned page data to a page.
    ///
    /// Implementations can override this to adopt owned page buffers without
    /// routing through a borrowed slice first.
    fn write_page_data(&mut self, cx: &Cx, page_no: PageNumber, data: PageData) -> Result<()> {
        self.write_page(cx, page_no, data.as_bytes())
    }

    /// Allocate a new page.
    fn allocate_page(&mut self, cx: &Cx) -> Result<PageNumber>;
    /// Free a page.
    fn free_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<()>;
    /// Record a granular write witness for fine-grained SSI.
    fn record_write_witness(&mut self, cx: &Cx, key: WitnessKey);
}

// ---------------------------------------------------------------------------
// TransactionHandle adapter (pager -> btree)
// ---------------------------------------------------------------------------

/// Adapter implementing [`PageReader`] and [`PageWriter`] by forwarding to a
/// [`TransactionHandle`].
///
/// This is the glue layer that lets `BtCursor` operate directly on top of the
/// MVCC pager transaction surface without any intermediate page store.
#[derive(Debug)]
pub struct TransactionPageIo<'a, T: TransactionHandle + ?Sized> {
    txn: &'a mut T,
}

impl<'a, T: TransactionHandle + ?Sized> TransactionPageIo<'a, T> {
    /// Wrap a pager transaction handle for use as a B-tree page I/O backend.
    #[must_use]
    pub fn new(txn: &'a mut T) -> Self {
        Self { txn }
    }

    /// Access the underlying transaction handle immutably.
    pub fn txn(&self) -> &T {
        &*self.txn
    }

    /// Access the underlying transaction handle mutably.
    pub fn txn_mut(&mut self) -> &mut T {
        self.txn
    }
}

impl<T: TransactionHandle + ?Sized> PageReader for TransactionPageIo<'_, T> {
    fn read_page(&self, cx: &Cx, page_no: PageNumber) -> Result<Vec<u8>> {
        Ok(self.txn.get_page(cx, page_no)?.into_vec())
    }

    fn read_page_data(&self, cx: &Cx, page_no: PageNumber) -> Result<PageData> {
        self.txn.get_page(cx, page_no)
    }
}

impl<T: TransactionHandle + ?Sized> PageWriter for TransactionPageIo<'_, T> {
    fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
        self.txn.write_page(cx, page_no, data)
    }

    fn write_page_data(&mut self, cx: &Cx, page_no: PageNumber, data: PageData) -> Result<()> {
        self.txn.write_page_data(cx, page_no, data)
    }

    fn allocate_page(&mut self, cx: &Cx) -> Result<PageNumber> {
        self.txn.allocate_page(cx)
    }

    fn free_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<()> {
        self.txn.free_page(cx, page_no)
    }

    fn record_write_witness(&mut self, cx: &Cx, key: WitnessKey) {
        self.txn.record_write_witness(cx, key);
    }
}

// ---------------------------------------------------------------------------
// In-memory page store (for VDBE storage cursors)
// ---------------------------------------------------------------------------

/// Simple in-memory page store implementing [`PageReader`] and [`PageWriter`].
///
/// Used by the VDBE storage cursor path to build transient B-trees from
/// in-memory table data without requiring the full pager/VFS stack.
#[derive(Debug, Clone)]
pub struct MemPageStore {
    pages: std::collections::HashMap<u32, Vec<u8>, foldhash::fast::FixedState>,
    page_size: u32,
}

impl MemPageStore {
    /// Create a new empty page store with the given page size.
    #[must_use]
    pub fn new(page_size: u32) -> Self {
        Self {
            pages: std::collections::HashMap::with_hasher(foldhash::fast::FixedState::default()),
            page_size,
        }
    }

    /// Initialize an empty leaf-table root page at the given page number.
    ///
    /// Call this once before constructing a [`BtCursor`] that will insert
    /// rows into the store. Avoid page 1 for transient stores since the
    /// B-tree code applies a 100-byte header offset to page 1.
    #[allow(clippy::cast_possible_truncation)]
    pub fn init_leaf_table_root(&mut self, pgno: PageNumber) {
        let mut page = vec![0u8; self.page_size as usize];
        page[0] = 0x0D;
        page[3..5].copy_from_slice(&0u16.to_be_bytes());
        let content_off = self.page_size as u16;
        page[5..7].copy_from_slice(&content_off.to_be_bytes());
        self.pages.insert(pgno.get(), page);
    }

    /// Create a page store pre-initialized with an empty leaf table B-tree
    /// at the given root page number.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn with_empty_table(root_page: PageNumber, page_size: u32) -> Self {
        let mut store = Self::new(page_size);
        let mut page = vec![0u8; page_size as usize];
        // Initialize as empty leaf table page (type 0x0D).
        page[0] = 0x0D;
        // Bytes 1-2: first freeblock offset = 0 (none).
        // Bytes 3-4: cell count = 0.
        // Bytes 5-6: content area offset = page_size (no cells yet).
        let content_offset = page_size as u16;
        page[5..7].copy_from_slice(&content_offset.to_be_bytes());
        // Byte 7: fragmented free bytes = 0.
        store.pages.insert(root_page.get(), page);
        store
    }

    pub fn with_empty_index(root_page: PageNumber, page_size: u32) -> Self {
        let mut store = Self::new(page_size);
        let mut page = vec![0u8; page_size as usize];
        // Initialize as empty leaf index page (type 0x0A).
        page[0] = 0x0A;
        // Bytes 1-2: first freeblock offset = 0 (none).
        // Bytes 3-4: cell count = 0.
        // Bytes 5-6: content area offset = page_size (no cells yet).
        let content_offset = page_size as u16;
        page[5..7].copy_from_slice(&content_offset.to_be_bytes());
        // Byte 7: fragmented free bytes = 0.
        store.pages.insert(root_page.get(), page);
        store
    }
}

impl PageReader for MemPageStore {
    fn read_page(&self, _cx: &Cx, page_no: PageNumber) -> Result<Vec<u8>> {
        self.pages
            .get(&page_no.get())
            .cloned()
            .ok_or_else(|| FrankenError::internal("page not found"))
    }
}

impl PageWriter for MemPageStore {
    fn write_page(&mut self, _cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
        let page_size = self.page_size as usize;
        if data.len() > page_size {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "test page store refused oversized page write: {} > {}",
                    data.len(),
                    page_size
                ),
            });
        }
        let mut page = vec![0_u8; page_size];
        let copy_len = data.len().min(page_size);
        page[..copy_len].copy_from_slice(&data[..copy_len]);
        self.pages.insert(page_no.get(), page);
        Ok(())
    }

    fn write_page_data(&mut self, _cx: &Cx, page_no: PageNumber, data: PageData) -> Result<()> {
        let page_size = self.page_size as usize;
        if data.len() > page_size {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "test page store refused oversized page write: {} > {}",
                    data.len(),
                    page_size
                ),
            });
        }
        let mut page = vec![0_u8; page_size];
        let copy_len = data.len().min(page_size);
        page[..copy_len].copy_from_slice(&data.as_bytes()[..copy_len]);
        self.pages.insert(page_no.get(), page);
        Ok(())
    }

    fn allocate_page(&mut self, _cx: &Cx) -> Result<PageNumber> {
        let next = self
            .pages
            .keys()
            .copied()
            .max()
            .unwrap_or(1)
            .checked_add(1)
            .ok_or(FrankenError::DatabaseFull)?;
        let pgno = PageNumber::new(next).ok_or(FrankenError::DatabaseFull)?;
        self.pages.insert(next, vec![0u8; self.page_size as usize]);
        Ok(pgno)
    }

    fn free_page(&mut self, _cx: &Cx, page_no: PageNumber) -> Result<()> {
        self.pages.remove(&page_no.get());
        Ok(())
    }

    fn record_write_witness(&mut self, _cx: &Cx, _key: WitnessKey) {}
}

// ---------------------------------------------------------------------------
// Cursor stack entry
// ---------------------------------------------------------------------------

/// A single entry in the cursor's page stack.
#[derive(Debug, Clone)]
struct StackEntry {
    /// Page number of this page (retained for debugging and future use in
    /// mutation operations).
    #[allow(dead_code)]
    page_no: PageNumber,
    /// Cached raw page data.
    page_data: PageData,
    /// Parsed page header.
    header: BtreePageHeader,
    /// Cell pointer offsets (cached from the cell pointer array).
    cell_pointers: Arc<[u16]>,
    /// Current cell index. For interior pages, this indicates which child
    /// was descended into. For leaf pages, this is the current position.
    /// A value equal to `cell_count` means "past the right-most child" on
    /// interior pages, or "past the last cell" on leaf pages.
    cell_idx: u16,
}

// ---------------------------------------------------------------------------
// BtCursor
// ---------------------------------------------------------------------------

/// A B-tree cursor that navigates through B-tree pages using a page stack.
///
/// Generic over the page I/O backend for testability.
#[derive(Debug)]
pub struct BtCursor<P> {
    /// Page I/O backend.
    pager: P,
    /// Root page number of the B-tree.
    root_page: PageNumber,
    /// Usable page size (page_size - reserved_bytes).
    usable_size: u32,
    /// Full page size on disk (usable_size + reserved_bytes).
    ///
    /// Pages written to disk must always be this size. Defaults to
    /// `usable_size` (i.e. reserved_bytes == 0) when not explicitly set.
    page_size: u32,
    /// Whether this is a table (intkey) or index (blobkey) B-tree.
    is_table: bool,
    /// Per-key descending flags for index cursors.
    ///
    /// The implicit trailing rowid suffix always sorts ascending, so this
    /// vector covers only the logical key terms before the rowid.
    index_desc_flags: Vec<bool>,
    /// Page stack from root to current leaf.
    stack: Vec<StackEntry>,
    /// Whether the cursor is at EOF (past the last entry).
    at_eof: bool,
    /// Read witnesses collected for SSI evidence.
    read_witnesses: Vec<WitnessKey>,
    /// Active per-operation observability stats while a `btree_op` span is open.
    active_op_stats: Option<BtreeOpRuntimeStats>,
    /// Reusable buffer for cell encoding — avoids per-insert heap allocation.
    ///
    /// Taken out of the struct via `std::mem::take` during encoding (to
    /// satisfy the borrow checker), then put back after insert completes.
    /// The Vec capacity is preserved across the take/put cycle so repeated
    /// inserts reuse the same allocation.
    cell_buf: Vec<u8>,
    /// Last rowid successfully inserted via `table_insert`.
    ///
    /// Set on successful leaf insert or balance-for-insert.  Used by the VDBE
    /// engine to implement `sqlite3_last_insert_rowid()` on a per-cursor basis.
    pub last_insert_rowid: Option<i64>,
    /// bd-wwqen.3: Cached (leaf_page, last_rowid) from the most recent
    /// successful hinted-leaf append. On the next hinted-leaf insert, if the
    /// hint matches this cached page AND rowid > cached_rowid, skip the
    /// `self.rowid(cx)?` cell parse and use the cached value directly.
    /// Cleared on any non-append cursor operation.
    append_hint_cache: Option<(PageNumber, i64)>,
    /// Sequential-seek cache: (leaf_page, last_rowid) from the most recent
    /// successful seek. On the next seek, if target_rowid > last_rowid, try
    /// the cached leaf first to skip root-to-leaf descent. Cleared on any
    /// non-forward seek or tree modification.
    seek_cache: Option<(PageNumber, i64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum FirstIndexKeyIntegerLocalRunSegment {
    Matched(i64),
    Mismatch { matched: i64, current_value: i64 },
    NeedsFallback { matched: i64 },
}

impl<P> BtCursor<P> {
    /// Force the cursor into EOF state (not positioned on any row).
    ///
    /// Used by `OP_NullRow` to ensure subsequent `Column`/`Rowid` reads
    /// return NULL without having to navigate the B-tree.
    pub fn invalidate(&mut self) {
        self.at_eof = true;
        self.stack.clear();
        self.append_hint_cache = None;
        self.seek_cache = None;
    }

    /// Whether this cursor is for a table (intkey) B-tree.
    #[must_use]
    pub fn is_table(&self) -> bool {
        self.is_table
    }

    /// The root page number of the B-tree this cursor operates on.
    #[must_use]
    pub fn root_page(&self) -> PageNumber {
        self.root_page
    }

    /// Lightweight identity for the cursor's current logical position.
    ///
    /// Used by the VDBE to cache decoded row state while the cursor remains
    /// on the same leaf cell.
    #[must_use]
    pub fn position_stamp(&self) -> Option<(u32, u16)> {
        if self.at_eof {
            return None;
        }

        self.stack
            .last()
            .map(|entry| (entry.page_no.get(), entry.cell_idx))
    }

    /// The usable page size for this cursor's B-tree.
    #[must_use]
    pub fn usable_size(&self) -> u32 {
        self.usable_size
    }

    /// The full on-disk page size (usable_size + reserved_bytes).
    #[must_use]
    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// Set the full on-disk page size when reserved_bytes > 0.
    ///
    /// By default `page_size == usable_size` (no reserved bytes). Call this
    /// after construction when the database header specifies a non-zero
    /// reserved-byte count so that newly built pages are allocated at the
    /// correct full page size.
    pub fn set_page_size(&mut self, page_size: u32) {
        debug_assert!(
            page_size >= self.usable_size,
            "page_size ({page_size}) must be >= usable_size ({})",
            self.usable_size
        );
        self.page_size = page_size;
    }
}

impl<P: PageReader> BtCursor<P> {
    fn first_index_key_integer_from_local_payload(local: &[u8]) -> Result<Option<i64>> {
        if local.is_empty() {
            return Ok(None);
        }

        let (header_size_u64, hdr_varint_len) = match read_varint(local) {
            Some(parsed) => parsed,
            None => return Ok(None),
        };
        let header_size = match usize::try_from(header_size_u64) {
            Ok(size) => size,
            Err(_) => return Ok(None),
        };
        if header_size < hdr_varint_len || header_size > local.len() {
            return Ok(None);
        }

        let (serial_type, _) = match read_varint(&local[hdr_varint_len..header_size]) {
            Some(parsed) => parsed,
            None => return Ok(None),
        };
        let value_len = match serial_type_len(serial_type).and_then(|len| usize::try_from(len).ok())
        {
            Some(len) => len,
            None => return Ok(None),
        };
        let body_offset = header_size;
        let col_end = match body_offset.checked_add(value_len) {
            Some(end) => end,
            None => return Ok(None),
        };
        if col_end > local.len() {
            return Ok(None);
        }

        Ok(Some(match classify_serial_type(serial_type) {
            SerialTypeClass::Zero => 0,
            SerialTypeClass::One => 1,
            SerialTypeClass::Integer => decode_big_endian_signed_fast(&local[body_offset..col_end]),
            _ => return Ok(None),
        }))
    }

    fn first_index_key_integer_local_value_from_cell_offset(
        &self,
        page: &[u8],
        page_type: cell::BtreePageType,
        cell_offset: usize,
    ) -> Result<Option<i64>> {
        if page_type.is_table() {
            return Ok(None);
        }

        let mut pos = cell_offset;
        if page_type.is_interior() {
            if pos + 4 > page.len() {
                return Ok(None);
            }
            pos += 4;
        }

        let (payload_size_raw, payload_varint_len) = match page.get(pos..) {
            Some(rest) => match read_varint(rest) {
                Some(parsed) => parsed,
                None => return Ok(None),
            },
            None => return Ok(None),
        };
        let payload_size = match u32::try_from(payload_size_raw) {
            Ok(size) => size,
            Err(_) => return Ok(None),
        };
        pos = match pos.checked_add(payload_varint_len) {
            Some(next) => next,
            None => return Ok(None),
        };

        let local_size =
            cell::local_payload_size(payload_size, self.usable_size, page_type) as usize;
        let local_end = match pos.checked_add(local_size) {
            Some(end) => end,
            None => return Ok(None),
        };
        if local_end > page.len() || local_end > self.usable_size as usize {
            return Ok(None);
        }

        Self::first_index_key_integer_from_local_payload(&page[pos..local_end])
    }

    fn index_cell_first_key_integer_local_value_at(
        &self,
        entry: &StackEntry,
        cell_idx: u16,
    ) -> Result<Option<i64>> {
        let idx_usize = cell_idx as usize;
        if idx_usize >= entry.cell_pointers.len() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "cell index {} out of bounds ({})",
                    cell_idx,
                    entry.cell_pointers.len()
                ),
            });
        }

        let cell_offset = entry.cell_pointers[idx_usize] as usize;
        self.first_index_key_integer_local_value_from_cell_offset(
            entry.page_data.as_bytes(),
            entry.header.page_type,
            cell_offset,
        )
    }

    fn current_first_index_key_integer_local_value(&self) -> Result<Option<i64>> {
        if self.at_eof || self.stack.is_empty() {
            return Err(FrankenError::internal("cursor at EOF"));
        }
        let top = self
            .stack
            .last()
            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?;
        self.index_cell_first_key_integer_local_value_at(top, top.cell_idx)
    }

    /// Probe the current index cell's first key column as an integer using
    /// only the local payload bytes already resident on the leaf page.
    ///
    /// Returns `Ok(Some(...))` when the first column is fully available from
    /// the local payload and is integer-class (`0`, `1`, or integer serial
    /// types). Returns `Ok(None)` when the caller must fall back to the slower
    /// prefix-buffer path (for example because the first field spans overflow
    /// or is non-integer-class).
    pub fn try_probe_current_first_index_key_integer_local(
        &self,
        probe_value: i64,
    ) -> Result<Option<(bool, i64)>> {
        Ok(self
            .current_first_index_key_integer_local_value()?
            .map(|current_value| (current_value == probe_value, current_value)))
    }

    /// Count a matched segment of integer first-key entries while the cursor
    /// stays on a leaf page and the first column is fully available from local
    /// payload bytes. The cursor is left on the first unconsumed row
    /// (mismatch/fallback) or advanced once to the next logical row/eof after
    /// the matched local segment.
    pub fn count_equal_first_index_key_run_integer_local_segment(
        &mut self,
        cx: &Cx,
        probe_value: i64,
    ) -> Result<FirstIndexKeyIntegerLocalRunSegment> {
        enum LocalRunScanOutcome {
            MatchedAll(i64),
            MatchedCurrent(i64),
            Mismatch {
                matched: i64,
                cell_idx: Option<u16>,
                current_value: i64,
            },
            NeedsFallback {
                matched: i64,
                cell_idx: Option<u16>,
            },
        }

        let mut matched_total = 0_i64;
        loop {
            if self.at_eof || self.stack.is_empty() {
                return Ok(FirstIndexKeyIntegerLocalRunSegment::Matched(matched_total));
            }

            let scan_outcome = {
                let top = self
                    .stack
                    .last()
                    .ok_or_else(|| FrankenError::internal("cursor stack empty"))?;
                let page_type = top.header.page_type;
                if page_type.is_leaf() && !page_type.is_table() {
                    let start_idx = top.cell_idx;
                    let cell_count = top.header.cell_count;
                    let page = top.page_data.as_bytes();
                    let cell_pointers = &top.cell_pointers;
                    let mut matched = 0_i64;
                    let mut outcome = None;

                    for idx in start_idx..cell_count {
                        let cell_offset =
                            cell_pointers.get(idx as usize).copied().ok_or_else(|| {
                                FrankenError::DatabaseCorrupt {
                                    detail: format!(
                                        "cell index {} out of bounds ({})",
                                        idx,
                                        cell_pointers.len()
                                    ),
                                }
                            })? as usize;

                        match self.first_index_key_integer_local_value_from_cell_offset(
                            page,
                            page_type,
                            cell_offset,
                        )? {
                            Some(value) if value == probe_value => {
                                matched = matched.wrapping_add(1);
                            }
                            Some(current_value) => {
                                outcome = Some(LocalRunScanOutcome::Mismatch {
                                    matched,
                                    cell_idx: Some(idx),
                                    current_value,
                                });
                                break;
                            }
                            None => {
                                outcome = Some(LocalRunScanOutcome::NeedsFallback {
                                    matched,
                                    cell_idx: Some(idx),
                                });
                                break;
                            }
                        }
                    }

                    outcome.unwrap_or(LocalRunScanOutcome::MatchedAll(matched))
                } else if page_type.is_table() {
                    LocalRunScanOutcome::NeedsFallback {
                        matched: 0,
                        cell_idx: Some(top.cell_idx),
                    }
                } else {
                    match self.current_first_index_key_integer_local_value()? {
                        Some(current_value) if current_value == probe_value => {
                            LocalRunScanOutcome::MatchedCurrent(1)
                        }
                        Some(current_value) => LocalRunScanOutcome::Mismatch {
                            matched: 0,
                            cell_idx: None,
                            current_value,
                        },
                        None => LocalRunScanOutcome::NeedsFallback {
                            matched: 0,
                            cell_idx: None,
                        },
                    }
                }
            };

            match scan_outcome {
                LocalRunScanOutcome::MatchedAll(matched)
                | LocalRunScanOutcome::MatchedCurrent(matched) => {
                    matched_total = matched_total.wrapping_add(matched);
                    if !self.advance_next(cx)? {
                        return Ok(FirstIndexKeyIntegerLocalRunSegment::Matched(matched_total));
                    }
                }
                LocalRunScanOutcome::Mismatch {
                    matched,
                    cell_idx,
                    current_value,
                } => {
                    if let Some(cell_idx) = cell_idx {
                        self.stack
                            .last_mut()
                            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?
                            .cell_idx = cell_idx;
                    }
                    return Ok(FirstIndexKeyIntegerLocalRunSegment::Mismatch {
                        matched: matched_total.wrapping_add(matched),
                        current_value,
                    });
                }
                LocalRunScanOutcome::NeedsFallback { matched, cell_idx } => {
                    if let Some(cell_idx) = cell_idx {
                        self.stack
                            .last_mut()
                            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?
                            .cell_idx = cell_idx;
                    }
                    return Ok(FirstIndexKeyIntegerLocalRunSegment::NeedsFallback {
                        matched: matched_total.wrapping_add(matched),
                    });
                }
            }
        }
    }

    /// Create a new cursor positioned before the first entry (at EOF).
    #[must_use]
    pub fn new(pager: P, root_page: PageNumber, usable_size: u32, is_table: bool) -> Self {
        Self::new_with_index_desc(pager, root_page, usable_size, is_table, Vec::new())
    }

    /// Create a new cursor with explicit descending metadata for index keys.
    #[must_use]
    pub fn new_with_index_desc(
        pager: P,
        root_page: PageNumber,
        usable_size: u32,
        is_table: bool,
        index_desc_flags: Vec<bool>,
    ) -> Self {
        Self {
            pager,
            root_page,
            usable_size,
            page_size: usable_size,
            is_table,
            index_desc_flags,
            stack: Vec::with_capacity(BTREE_MAX_DEPTH as usize),
            at_eof: true,
            read_witnesses: Vec::new(),
            active_op_stats: None,
            cell_buf: Vec::new(),
            last_insert_rowid: None,
            append_hint_cache: None,
            seek_cache: None,
        }
    }

    /// Returns the read witness keys captured by the cursor.
    #[must_use]
    pub fn witness_keys(&self) -> &[WitnessKey] {
        &self.read_witnesses
    }

    /// Clears captured read witness keys.
    pub fn clear_witness_keys(&mut self) {
        self.read_witnesses.clear();
    }

    /// Return the current leaf page when positioned on a row.
    ///
    /// Returns `None` when the cursor is at EOF or not yet positioned.
    #[must_use]
    pub fn current_page(&self) -> Option<PageNumber> {
        if self.at_eof {
            return None;
        }
        self.stack.last().map(|entry| entry.page_no)
    }

    /// Issue an explicit best-effort prefetch hint for `page_no`.
    ///
    /// This is a non-blocking hint only; callers must not rely on it for
    /// correctness.
    pub fn prefetch_page_hint(&self, cx: &Cx, page_no: PageNumber) {
        self.issue_prefetch_hint(cx, page_no);
    }

    #[inline]
    fn cell_tag_from_rowid(rowid: i64) -> u64 {
        u64::from_ne_bytes(rowid.to_ne_bytes())
    }

    fn cell_tag_from_index_key(key: &[u8]) -> u64 {
        // Deterministic FNV-1a hash keeps tags stable across runs.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for &byte in key {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3_u64);
        }
        hash
    }

    fn record_point_witness(&mut self, key: WitnessKey) {
        if let WitnessKey::Page(page) = key {
            warn!(
                root_page = self.root_page.get(),
                page = page.get(),
                "policy violation: point operation emitted page-level witness"
            );
        }
        self.read_witnesses.push(key);
    }

    fn record_range_page_witness(&mut self, page_no: PageNumber) {
        self.read_witnesses.push(WitnessKey::Page(page_no));
    }

    fn issue_prefetch_hint(&self, cx: &Cx, page_no: PageNumber) {
        self.pager.prefetch_page_hint(cx, page_no);
        debug!(
            page_number = page_no.get(),
            source = "btree_descent",
            "issued best-effort btree prefetch hint"
        );
    }

    fn note_page_visit(&mut self, page_no: PageNumber) {
        if let Some(stats) = self.active_op_stats.as_mut() {
            stats.record_page_visit();
            trace!(page_number = page_no.get(), "btree page visit");
        }
    }

    fn note_split_event(&mut self) {
        instrumentation::record_split_event();
        if let Some(stats) = self.active_op_stats.as_mut() {
            stats.record_split();
        }
    }

    fn note_merge_event(&mut self) {
        if let Some(stats) = self.active_op_stats.as_mut() {
            stats.record_merge();
        }
    }

    fn measure_tree_depth(&mut self, cx: &Cx) -> Result<usize> {
        let saved_stats = self.active_op_stats.take();
        let mut depth = 0usize;
        let mut current_page = self.root_page;

        let result = loop {
            if depth >= BTREE_MAX_DEPTH as usize {
                break Err(FrankenError::DatabaseCorrupt {
                    detail: format!("B-tree depth exceeds maximum of {}", BTREE_MAX_DEPTH),
                });
            }

            let entry = match self.load_page(cx, current_page) {
                Ok(e) => e,
                Err(err) => break Err(err),
            };
            depth = depth.saturating_add(1);
            if entry.header.page_type.is_leaf() {
                break Ok(depth);
            }

            current_page = if entry.header.cell_count == 0 {
                match entry
                    .header
                    .right_child
                    .ok_or_else(|| FrankenError::DatabaseCorrupt {
                        detail: "interior page has no right child".to_owned(),
                    }) {
                    Ok(p) => p,
                    Err(err) => break Err(err),
                }
            } else {
                match self.parse_cell_at(&entry, 0) {
                    Ok(cell) => match cell
                        .left_child
                        .ok_or_else(|| FrankenError::DatabaseCorrupt {
                            detail: "interior cell has no left child".to_owned(),
                        }) {
                        Ok(p) => p,
                        Err(err) => break Err(err),
                    },
                    Err(err) => break Err(err),
                }
            };
        };

        self.active_op_stats = saved_stats;
        result
    }

    /// Count all rows in this table B-tree without decoding cell payloads.
    ///
    /// Walks every leaf page summing `cell_count` values. This avoids key
    /// parsing, overflow chain following, and register materialization — it
    /// only reads page headers. Still O(pages) but with a much smaller
    /// constant factor than the `first()/while next()` row-by-row scan.
    ///
    /// bd-wwqen.1 (B1.1): direct COUNT(*) fast path.
    pub fn count_all_rows(&mut self, cx: &Cx) -> Result<i64> {
        // Save and restore cursor state so this doesn't disturb
        // any in-progress iteration.
        let saved_eof = self.at_eof;
        self.stack.clear();
        self.at_eof = true;

        // bd-wwqen.1: iterative B-tree walk — no recursion overhead.
        let result = self.count_all_rows_iterative(cx);

        // Restore cursor state.
        self.stack.clear();
        self.at_eof = saved_eof;

        result
    }

    /// bd-wwqen.1: Iterative B-tree count matching SQLite's sqlite3BtreeCount
    /// pattern. Walks every page exactly once without recursion, extracting
    /// child page numbers directly from raw cell bytes (4-byte BE at cell
    /// offset) to avoid full parse_cell_at overhead.
    fn count_all_rows_iterative(&mut self, cx: &Cx) -> Result<i64> {
        // Stack: (page_no, next_cell_idx, cell_count, right_child, header_size).
        // Zero-allocation hot path: reads page data + header only, no Vec<u16>
        // cell_pointers per page.
        let mut visit_stack: Vec<(PageNumber, u16, u16, Option<PageNumber>, usize)> = Vec::new();
        let mut total: i64 = 0;
        let mut current_page = self.root_page;

        loop {
            observe_cursor_cancellation(cx)?;
            self.note_page_visit(current_page);

            let page_data = self.pager.read_page_data(cx, current_page)?;
            let page_bytes = page_data.as_bytes();
            let header = cell::parse_page_header(page_bytes, current_page)?;

            if header.page_type.is_leaf() {
                total = total.saturating_add(i64::from(header.cell_count));

                loop {
                    let Some((parent_page, cell_idx, cell_count, right_child, hdr_size)) =
                        visit_stack.last_mut()
                    else {
                        return Ok(total);
                    };

                    if *cell_idx < *cell_count {
                        let parent_data = self.pager.read_page_data(cx, *parent_page)?;
                        let parent_bytes = parent_data.as_bytes();
                        let cell_ptr = Self::read_cell_pointer_inline(
                            parent_bytes,
                            *parent_page,
                            *hdr_size,
                            *cell_idx,
                        )?;
                        let child = Self::read_child_at_offset(parent_bytes, cell_ptr as usize)?;
                        *cell_idx += 1;
                        current_page = child;
                        break;
                    } else if let Some(rc) = right_child.take() {
                        current_page = rc;
                        break;
                    } else {
                        visit_stack.pop();
                    }
                }
            } else {
                let cell_count = header.cell_count;
                let hdr_size = header.page_type.header_size() as usize;
                let right_child =
                    header
                        .right_child
                        .ok_or_else(|| FrankenError::DatabaseCorrupt {
                            detail: "interior page has no right child in count_all_rows".to_owned(),
                        })?;

                // Index interior separator cells are logical entries in this
                // implementation, and cursor next/prev traversal already visits
                // them directly. COUNT on an index root must include them too.
                if !self.is_table {
                    total = total.saturating_add(i64::from(cell_count));
                }

                if cell_count == 0 {
                    visit_stack.push((current_page, 0, 0, None, hdr_size));
                    current_page = right_child;
                } else {
                    let cell_ptr =
                        Self::read_cell_pointer_inline(page_bytes, current_page, hdr_size, 0)?;
                    let first_child = Self::read_child_at_offset(page_bytes, cell_ptr as usize)?;
                    visit_stack.push((current_page, 1, cell_count, Some(right_child), hdr_size));
                    current_page = first_child;
                }
            }
        }
    }

    /// Extract the left_child page number from an interior cell at the given
    /// index without full cell parsing. Interior cells store a 4-byte BE
    /// page number at the start of the cell.
    fn read_interior_cell_child(&self, entry: &StackEntry, cell_idx: u16) -> Result<PageNumber> {
        let cell_pointers = &entry.cell_pointers;
        let idx = cell_idx as usize;
        if idx >= cell_pointers.len() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "cell index {idx} out of range for page with {} cells",
                    cell_pointers.len()
                ),
            });
        }
        let cell_offset = cell_pointers[idx] as usize;
        Self::read_child_at_offset(entry.page_data.as_bytes(), cell_offset)
    }

    /// bd-wwqen.1: Read a 4-byte BE child page number directly from raw page
    /// bytes at the given cell offset. Used by count_all_rows_iterative to
    /// avoid needing the allocated cell_pointers Vec.
    fn read_child_at_offset(page: &[u8], cell_offset: usize) -> Result<PageNumber> {
        if cell_offset + 4 > page.len() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: "interior cell extends past page in count_all_rows".to_owned(),
            });
        }
        let pgno = u32::from_be_bytes([
            page[cell_offset],
            page[cell_offset + 1],
            page[cell_offset + 2],
            page[cell_offset + 3],
        ]);
        PageNumber::new(pgno).ok_or_else(|| FrankenError::DatabaseCorrupt {
            detail: "interior cell has zero left-child pointer in count_all_rows".to_owned(),
        })
    }

    /// bd-wwqen.1: Read the cell pointer at index `cell_idx` directly from
    /// raw page bytes without allocating a Vec<u16>. The cell pointer array
    /// starts right after the page header.
    fn read_cell_pointer_inline(
        page: &[u8],
        page_no: PageNumber,
        header_size: usize,
        cell_idx: u16,
    ) -> Result<u16> {
        let header_offset = cell::header_offset_for_page(page_no);
        let ptr_offset = header_offset + header_size + (cell_idx as usize) * 2;
        if ptr_offset + 2 > page.len() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!("cell pointer {cell_idx} extends past page in count_all_rows"),
            });
        }
        Ok(u16::from_be_bytes([page[ptr_offset], page[ptr_offset + 1]]))
    }

    fn record_depth_gauge(&mut self, cx: &Cx) -> Result<()> {
        let depth = if self.stack.is_empty() {
            self.measure_tree_depth(cx)?
        } else {
            self.stack.len()
        };
        instrumentation::set_depth_gauge(depth);
        Ok(())
    }

    fn with_btree_op<T, F>(&mut self, cx: &Cx, op_type: BtreeOpType, work: F) -> Result<T>
    where
        F: FnOnce(&mut Self) -> Result<T>,
    {
        instrumentation::record_operation(op_type);
        let span = tracing::span!(
            Level::DEBUG,
            "btree_op",
            op_type = op_type.as_str(),
            pages_visited = tracing::field::Empty,
            splits = tracing::field::Empty,
            merges = tracing::field::Empty
        );
        let _entered = span.enter();
        debug!(op_type = op_type.as_str(), "starting btree operation");

        self.active_op_stats = Some(BtreeOpRuntimeStats::default());
        let result = work(self);

        if let Err(error) = self.record_depth_gauge(cx) {
            debug!(
                op_type = op_type.as_str(),
                error = %error,
                "failed to refresh btree depth gauge"
            );
        }

        let stats = self.active_op_stats.take().unwrap_or_default();
        span.record("pages_visited", stats.pages_visited);
        span.record("splits", stats.splits);
        span.record("merges", stats.merges);

        if let Err(error) = &result {
            debug!(
                op_type = op_type.as_str(),
                pages_visited = stats.pages_visited,
                splits = stats.splits,
                merges = stats.merges,
                error = %error,
                "btree operation failed"
            );
        } else {
            debug!(
                op_type = op_type.as_str(),
                pages_visited = stats.pages_visited,
                splits = stats.splits,
                merges = stats.merges,
                "btree operation completed"
            );
        }

        result
    }

    /// Load a page into a stack entry.
    fn load_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<StackEntry> {
        observe_cursor_cancellation(cx)?;

        // Alien Optimization: Hot-path Stack Elision.
        // We can only elide the load if the page hasn't been modified
        // in the current transaction (is not dirty).
        if let Some(existing) = self.stack.last() {
            if existing.page_no == page_no && !self.pager.is_dirty(page_no) {
                // In MVCC, unmodified pages for a given snapshot are immutable.
                let mut cached = existing.clone();
                cached.cell_idx = 0;
                self.note_page_visit(page_no);
                return Ok(cached);
            }
        }

        self.note_page_visit(page_no);
        let page_data = self.pager.read_page_data(cx, page_no)?;
        let header_offset = cell::header_offset_for_page(page_no);
        let header = cell::parse_page_header(page_data.as_bytes(), page_no)?;
        let cell_pointers = Arc::<[u16]>::from(cell::read_cell_pointers(
            page_data.as_bytes(),
            &header,
            header_offset,
        )?);

        Ok(StackEntry {
            page_no,
            page_data,
            header,
            cell_pointers,
            cell_idx: 0,
        })
    }

    /// Reload a page from the pager, bypassing the stack-entry cache.
    ///
    /// This is required immediately after in-place writes because some test
    /// pagers do not surface dirty-state through `is_dirty()`.
    fn reload_page_fresh(&mut self, cx: &Cx, page_no: PageNumber) -> Result<StackEntry> {
        observe_cursor_cancellation(cx)?;
        self.note_page_visit(page_no);
        let page_data = self.pager.read_page_data(cx, page_no)?;
        let header_offset = cell::header_offset_for_page(page_no);
        let header = cell::parse_page_header(page_data.as_bytes(), page_no)?;
        let cell_pointers = Arc::<[u16]>::from(cell::read_cell_pointers(
            page_data.as_bytes(),
            &header,
            header_offset,
        )?);
        Ok(StackEntry {
            page_no,
            page_data,
            header,
            cell_pointers,
            cell_idx: 0,
        })
    }

    /// Parse a cell at the given index on the top-of-stack page.
    fn parse_cell_at(&self, entry: &StackEntry, idx: u16) -> Result<CellRef> {
        let idx_usize = idx as usize;
        if idx_usize >= entry.cell_pointers.len() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "cell index {} out of bounds ({})",
                    idx,
                    entry.cell_pointers.len()
                ),
            });
        }
        let offset = entry.cell_pointers[idx_usize] as usize;
        CellRef::parse(
            entry.page_data.as_bytes(),
            offset,
            entry.header.page_type,
            self.usable_size,
        )
    }

    /// Move the cursor to the first entry in the subtree rooted at `page_no`.
    fn move_to_leftmost_leaf(
        &mut self,
        cx: &Cx,
        page_no: PageNumber,
        record_leaf_witness: bool,
    ) -> Result<bool> {
        let mut current_page = page_no;
        loop {
            observe_cursor_cancellation(cx)?;
            if self.stack.len() >= BTREE_MAX_DEPTH as usize {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("B-tree depth exceeds maximum of {}", BTREE_MAX_DEPTH),
                });
            }

            let mut entry = self.load_page(cx, current_page)?;
            entry.cell_idx = 0;

            if entry.header.page_type.is_leaf() {
                let leaf_page_no = entry.page_no;
                if entry.header.cell_count == 0 {
                    self.stack.push(entry);
                    self.at_eof = true;
                    if record_leaf_witness {
                        self.record_range_page_witness(leaf_page_no);
                    }
                    return Ok(false);
                }
                self.stack.push(entry);
                self.at_eof = false;
                if record_leaf_witness {
                    self.record_range_page_witness(leaf_page_no);
                }
                return Ok(true);
            }

            // Interior page: follow the leftmost child (cell 0's left child).
            if entry.header.cell_count == 0 {
                // Interior page with no cells — follow right_child.
                let right =
                    entry
                        .header
                        .right_child
                        .ok_or_else(|| FrankenError::DatabaseCorrupt {
                            detail: "interior page has no right child".to_owned(),
                        })?;
                entry.cell_idx = 0;
                self.stack.push(entry);
                self.issue_prefetch_hint(cx, right);
                current_page = right;
            } else {
                let cell = self.parse_cell_at(&entry, 0)?;
                let child = cell
                    .left_child
                    .ok_or_else(|| FrankenError::DatabaseCorrupt {
                        detail: "interior cell has no left child".to_owned(),
                    })?;
                entry.cell_idx = 0;
                self.stack.push(entry);
                self.issue_prefetch_hint(cx, child);
                current_page = child;
            }
        }
    }

    /// Move the cursor to the last entry in the subtree rooted at `page_no`.
    fn move_to_rightmost_leaf(
        &mut self,
        cx: &Cx,
        page_no: PageNumber,
        record_leaf_witness: bool,
    ) -> Result<bool> {
        let mut current_page = page_no;
        loop {
            observe_cursor_cancellation(cx)?;
            if self.stack.len() >= BTREE_MAX_DEPTH as usize {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("B-tree depth exceeds maximum of {}", BTREE_MAX_DEPTH),
                });
            }

            let mut entry = self.load_page(cx, current_page)?;

            if entry.header.page_type.is_leaf() {
                let leaf_page_no = entry.page_no;
                if entry.header.cell_count == 0 {
                    self.stack.push(entry);
                    self.at_eof = true;
                    if record_leaf_witness {
                        self.record_range_page_witness(leaf_page_no);
                    }
                    return Ok(false);
                }
                entry.cell_idx = entry.header.cell_count - 1;
                self.stack.push(entry);
                self.at_eof = false;
                if record_leaf_witness {
                    self.record_range_page_witness(leaf_page_no);
                }
                return Ok(true);
            }

            // Interior page: follow the right-most child.
            let right = entry
                .header
                .right_child
                .ok_or_else(|| FrankenError::DatabaseCorrupt {
                    detail: "interior page has no right child".to_owned(),
                })?;
            entry.cell_idx = entry.header.cell_count;
            self.stack.push(entry);
            self.issue_prefetch_hint(cx, right);
            current_page = right;
        }
    }

    /// Get the child page for an interior page at the given cell index.
    ///
    /// If `cell_idx == cell_count`, returns the right child.
    /// Otherwise, returns the left child of the cell at `cell_idx`.
    fn child_page_at(&self, entry: &StackEntry, cell_idx: u16) -> Result<PageNumber> {
        if cell_idx >= entry.header.cell_count {
            entry
                .header
                .right_child
                .ok_or_else(|| FrankenError::DatabaseCorrupt {
                    detail: "interior page has no right child".to_owned(),
                })
        } else {
            let cell = self.parse_cell_at(entry, cell_idx)?;
            cell.left_child
                .ok_or_else(|| FrankenError::DatabaseCorrupt {
                    detail: "interior cell has no left child".to_owned(),
                })
        }
    }

    /// Recompute which child slot in an ancestor page points at `child_page_no`.
    ///
    /// Upward split propagation cannot trust the stale cursor-stack `cell_idx`
    /// after a lower-level balance rewrites the tree shape underneath it.
    fn find_child_slot_by_page_no(
        &mut self,
        cx: &Cx,
        parent_page_no: PageNumber,
        child_page_no: PageNumber,
    ) -> Result<u16> {
        let entry = self.reload_page_fresh(cx, parent_page_no)?;
        if !entry.header.page_type.is_interior() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "cannot locate child slot in non-interior page {}",
                    parent_page_no
                ),
            });
        }

        for child_idx in 0..=entry.header.cell_count {
            if self.child_page_at(&entry, child_idx)? == child_page_no {
                return Ok(child_idx);
            }
        }

        Err(FrankenError::DatabaseCorrupt {
            detail: format!(
                "ancestor page {} has no child pointer to page {}",
                parent_page_no, child_page_no
            ),
        })
    }

    /// Seek to a rowid in a table B-tree. Returns the seek result.
    fn table_seek(&mut self, cx: &Cx, target_rowid: i64) -> Result<SeekResult> {
        let res = self.table_seek_for_insert(cx, target_rowid)?;
        if !res.is_found() && self.at_eof {
            // We fell off the right edge of the leaf.
            // Determine if there is a successor up the tree.
            let mut has_successor = false;
            for parent in self.stack.iter().rev().skip(1) {
                if parent.cell_idx < parent.header.cell_count {
                    has_successor = true;
                    break;
                }
            }

            if has_successor {
                // There is a successor. Reset eof and use advance_next to reach it.
                self.at_eof = false;
                let advanced = self.advance_next(cx)?;
                if !advanced {
                    self.at_eof = true;
                }
            }
        }
        Ok(res)
    }

    /// Internal seek used by INSERT that anchors the cursor on the leaf where
    /// the target belongs, even if it falls off the right edge.
    fn table_seek_for_insert(&mut self, cx: &Cx, target_rowid: i64) -> Result<SeekResult> {
        observe_cursor_cancellation(cx)?;

        // Fast path: sequential seek cache. If target > cached_rowid, try the
        // cached leaf directly to skip root-to-leaf descent.
        if let Some((cached_page, cached_rowid)) = self.seek_cache {
            if target_rowid > cached_rowid {
                let entry = self.load_page(cx, cached_page)?;
                if entry.header.page_type.is_leaf() && entry.header.page_type.is_table() {
                    // Binary search on the cached leaf to confirm target belongs here.
                    let result = Self::binary_search_table_leaf(cx, &entry, target_rowid)?;
                    match result {
                        BinarySearchResult::Found(idx) => {
                            self.stack.clear();
                            let mut entry = entry;
                            entry.cell_idx = idx;
                            self.stack.push(entry);
                            self.at_eof = false;
                            self.seek_cache = Some((cached_page, target_rowid));
                            self.record_point_witness(WitnessKey::Cell {
                                btree_root: self.root_page,
                                leaf_page: cached_page,
                                tag: Self::cell_tag_from_rowid(target_rowid),
                            });
                            return Ok(SeekResult::Found);
                        }
                        BinarySearchResult::NotFound(idx) => {
                            // Target not found, but check if it belongs at end of this leaf.
                            if idx >= entry.header.cell_count {
                                // Target is past the end of this leaf — it may belong on
                                // the next leaf. Fall through to full seek.
                            } else {
                                // Target belongs in the middle of this leaf.
                                self.stack.clear();
                                let mut entry = entry;
                                entry.cell_idx = idx;
                                self.stack.push(entry);
                                self.at_eof = false;
                                self.seek_cache = Some((cached_page, target_rowid));
                                self.record_point_witness(WitnessKey::Cell {
                                    btree_root: self.root_page,
                                    leaf_page: cached_page,
                                    tag: Self::cell_tag_from_rowid(target_rowid),
                                });
                                return Ok(SeekResult::NotFound);
                            }
                        }
                    }
                }
            }
        }

        self.stack.clear();
        let mut current_page = self.root_page;

        loop {
            observe_cursor_cancellation(cx)?;
            if self.stack.len() >= BTREE_MAX_DEPTH as usize {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("B-tree depth exceeds maximum of {}", BTREE_MAX_DEPTH),
                });
            }

            let entry = self.load_page(cx, current_page)?;

            // Guard: detect is_table vs actual page-type mismatch early.
            // If the cursor was opened with is_table=true but the page is
            // actually an index page, binary_search_table_* will fail with
            // "cell has no rowid". Catch this here with a clearer error.
            if !entry.header.page_type.is_table() {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "table_seek called on index page (type {:?}, page {}, root {}): \
                         cursor is_table flag likely incorrect",
                        entry.header.page_type, current_page, self.root_page
                    ),
                });
            }

            if entry.header.page_type.is_leaf() {
                // Binary search on the leaf page by rowid.
                let result = Self::binary_search_table_leaf(cx, &entry, target_rowid)?;
                match result {
                    BinarySearchResult::Found(idx) => {
                        let mut entry = entry;
                        entry.cell_idx = idx;
                        self.stack.push(entry);
                        self.at_eof = false;
                        // Cache leaf position for sequential seek optimization.
                        self.seek_cache = Some((current_page, target_rowid));
                        self.record_point_witness(WitnessKey::Cell {
                            btree_root: self.root_page,
                            leaf_page: current_page,
                            tag: Self::cell_tag_from_rowid(target_rowid),
                        });
                        return Ok(SeekResult::Found);
                    }
                    BinarySearchResult::NotFound(idx) => {
                        let mut entry = entry;
                        if idx >= entry.header.cell_count {
                            // Target is strictly greater than the last key on this leaf.
                            // Keep the path anchored to this right-most leaf and mark EOF,
                            // so callers (notably INSERT) still have a valid stack context.
                            entry.cell_idx = entry.header.cell_count.saturating_sub(1);
                            self.stack.push(entry);
                            self.at_eof = true;
                            // Cache for sequential append optimization.
                            self.seek_cache = Some((current_page, target_rowid));
                        } else {
                            entry.cell_idx = idx;
                            self.stack.push(entry);
                            self.at_eof = false;
                            // Cache for sequential seek optimization.
                            self.seek_cache = Some((current_page, target_rowid));
                        }
                        self.record_point_witness(WitnessKey::Cell {
                            btree_root: self.root_page,
                            leaf_page: current_page,
                            tag: Self::cell_tag_from_rowid(target_rowid),
                        });
                        return Ok(SeekResult::NotFound);
                    }
                }
            }

            // Interior table page: binary search to find which child to descend.
            let child_idx = Self::binary_search_table_interior(cx, &entry, target_rowid)?;
            let child = self.child_page_at(&entry, child_idx)?;
            let mut entry = entry;
            entry.cell_idx = child_idx;
            self.stack.push(entry);
            self.issue_prefetch_hint(cx, child);
            current_page = child;
        }
    }

    /// Binary search a leaf table page for a rowid.
    fn binary_search_table_leaf(
        cx: &Cx,
        entry: &StackEntry,
        target: i64,
    ) -> Result<BinarySearchResult> {
        let count = entry.header.cell_count;
        if count == 0 {
            return Ok(BinarySearchResult::NotFound(0));
        }

        let mut lo = 0u16;
        let mut hi = count;
        while lo < hi {
            observe_cursor_cancellation(cx)?;
            let mid = lo + (hi - lo) / 2;
            let offset = entry.cell_pointers[mid as usize] as usize;
            let cell_data = &entry.page_data.as_bytes()[offset..];
            let rowid = if let Some((_, n)) = read_varint(cell_data) {
                if let Some((r, _)) = read_varint(&cell_data[n..]) {
                    #[allow(clippy::cast_possible_wrap)]
                    let rowid_val = r as i64;
                    rowid_val
                } else {
                    return Err(FrankenError::DatabaseCorrupt {
                        detail: "table leaf cell has invalid rowid varint".to_owned(),
                    });
                }
            } else {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: "table leaf cell has invalid payload size varint".to_owned(),
                });
            };

            match rowid.cmp(&target) {
                std::cmp::Ordering::Equal => return Ok(BinarySearchResult::Found(mid)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(BinarySearchResult::NotFound(lo))
    }

    /// Binary search an interior table page to find which child to descend.
    ///
    /// Returns the child index (0..=cell_count). If the target is greater
    /// than all keys, returns cell_count (meaning follow right_child).
    fn binary_search_table_interior(cx: &Cx, entry: &StackEntry, target: i64) -> Result<u16> {
        let count = entry.header.cell_count;
        if count == 0 {
            return Ok(0); // Follow right_child.
        }

        // Interior table cells are sorted by rowid. We want the child
        // whose subtree may contain `target`.
        //
        // Cell[i] has left_child and rowid. The left_child subtree contains
        // rowids < cell[i].rowid. If target <= cell[i].rowid, descend into
        // left_child of cell[i]. If target > last cell's rowid, follow right_child.
        let mut lo = 0u16;
        let mut hi = count;
        while lo < hi {
            observe_cursor_cancellation(cx)?;
            let mid = lo + (hi - lo) / 2;
            let offset = entry.cell_pointers[mid as usize] as usize;
            let cell_data = &entry.page_data.as_bytes()[offset..];
            if cell_data.len() < 4 {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: "interior table cell too short for child pointer".to_owned(),
                });
            }
            let rowid = if let Some((r, _)) = read_varint(&cell_data[4..]) {
                #[allow(clippy::cast_possible_wrap)]
                let rowid_val = r as i64;
                rowid_val
            } else {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: "interior table cell has invalid rowid varint".to_owned(),
                });
            };

            if target <= rowid {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        Ok(lo)
    }

    /// Seek to a key in an index B-tree. Returns the seek result.
    fn index_seek(&mut self, cx: &Cx, target_key: &[u8]) -> Result<SeekResult> {
        let res = self.index_seek_for_insert(cx, target_key)?;
        if !res.is_found() && self.at_eof {
            // We fell off the right edge of the leaf.
            // Determine if there is a successor up the tree.
            let mut has_successor = false;
            for parent in self.stack.iter().rev().skip(1) {
                if parent.cell_idx < parent.header.cell_count {
                    has_successor = true;
                    break;
                }
            }

            if has_successor {
                // There is a successor. Reset eof and use advance_next to reach it.
                self.at_eof = false;
                let advanced = self.advance_next(cx)?;
                if !advanced {
                    self.at_eof = true;
                }
            }
        }
        Ok(res)
    }

    /// Internal seek used by INSERT that anchors the cursor on the leaf where
    /// the target belongs, even if it falls off the right edge.
    fn index_seek_for_insert(&mut self, cx: &Cx, target_key: &[u8]) -> Result<SeekResult> {
        observe_cursor_cancellation(cx)?;
        self.stack.clear();
        let mut current_page = self.root_page;

        loop {
            observe_cursor_cancellation(cx)?;
            if self.stack.len() >= BTREE_MAX_DEPTH as usize {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("B-tree depth exceeds maximum of {}", BTREE_MAX_DEPTH),
                });
            }

            let entry = self.load_page(cx, current_page)?;

            // Guard: detect is_table vs actual page-type mismatch early.
            // If the cursor was opened with is_table=false but the page is
            // actually a table page, catch this with a clear diagnostic.
            if entry.header.page_type.is_table() {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "index_seek called on table page (type {:?}, page {}, root {}): \
                         cursor is_table flag likely incorrect",
                        entry.header.page_type, current_page, self.root_page
                    ),
                });
            }

            if entry.header.page_type.is_leaf() {
                let result = self.binary_search_index_leaf(cx, &entry, target_key)?;
                match result {
                    BinarySearchResult::Found(idx) => {
                        let mut entry = entry;
                        entry.cell_idx = idx;
                        self.stack.push(entry);
                        self.at_eof = false;
                        self.record_point_witness(WitnessKey::Cell {
                            btree_root: self.root_page,
                            leaf_page: current_page,
                            tag: Self::cell_tag_from_index_key(target_key),
                        });
                        return Ok(SeekResult::Found);
                    }
                    BinarySearchResult::NotFound(idx) => {
                        let mut entry = entry;
                        if idx >= entry.header.cell_count {
                            // Target is strictly greater than the last key on this leaf.
                            // Keep the path anchored to this right-most leaf and mark EOF,
                            // so callers (notably INSERT) still have a valid stack context.
                            entry.cell_idx = entry.header.cell_count.saturating_sub(1);
                            self.stack.push(entry);
                            self.at_eof = true;
                        } else {
                            entry.cell_idx = idx;
                            self.stack.push(entry);
                            self.at_eof = false;
                        }
                        self.record_point_witness(WitnessKey::Cell {
                            btree_root: self.root_page,
                            leaf_page: current_page,
                            tag: Self::cell_tag_from_index_key(target_key),
                        });
                        return Ok(SeekResult::NotFound);
                    }
                }
            }

            // Interior index page: binary search to find which child to descend.
            let search_result = self.binary_search_index_interior(cx, &entry, target_key)?;
            match search_result {
                BinarySearchResult::Found(idx) => {
                    let mut entry = entry;
                    entry.cell_idx = idx;
                    self.stack.push(entry);
                    self.at_eof = false;
                    self.record_point_witness(WitnessKey::Cell {
                        btree_root: self.root_page,
                        leaf_page: current_page,
                        tag: Self::cell_tag_from_index_key(target_key),
                    });
                    return Ok(SeekResult::Found);
                }
                BinarySearchResult::NotFound(idx) => {
                    let child = self.child_page_at(&entry, idx)?;
                    let mut entry = entry;
                    entry.cell_idx = idx;
                    self.stack.push(entry);
                    self.issue_prefetch_hint(cx, child);
                    current_page = child;
                }
            }
        }
    }

    /// Read the full payload for a cell into the provided buffer.
    fn read_cell_payload_into(
        &self,
        cx: &Cx,
        entry: &StackEntry,
        cell: &CellRef,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let local = cell.local_payload(entry.page_data.as_bytes());

        if let Some(first_overflow) = cell.overflow_page {
            overflow::read_overflow_chain_into(
                local,
                first_overflow,
                cell.payload_size,
                self.usable_size,
                &mut |pgno| self.pager.read_page_data(cx, pgno).map(PageData::into_vec),
                out,
            )
        } else {
            out.clear();
            instrumentation::record_local_payload_copy(local.len());
            out.extend_from_slice(local);
            Ok(())
        }
    }

    /// Read only a prefix of a cell payload into the provided buffer.
    fn read_cell_payload_prefix_into(
        &self,
        cx: &Cx,
        entry: &StackEntry,
        cell: &CellRef,
        max_prefix_bytes: usize,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let local = cell.local_payload(entry.page_data.as_bytes());
        let target_size = usize::try_from(cell.payload_size)
            .unwrap_or(usize::MAX)
            .min(max_prefix_bytes);
        if target_size == 0 {
            out.clear();
            return Ok(());
        }

        if let Some(first_overflow) = cell.overflow_page {
            overflow::read_overflow_chain_prefix_into(
                local,
                first_overflow,
                cell.payload_size,
                self.usable_size,
                target_size,
                &mut |pgno| self.pager.read_page_data(cx, pgno).map(PageData::into_vec),
                out,
            )
        } else {
            out.clear();
            let local_copy_len = local.len().min(target_size);
            instrumentation::record_local_payload_copy(local_copy_len);
            out.extend_from_slice(&local[..local_copy_len]);
            Ok(())
        }
    }

    /// Read the full payload for a cell (resolving overflow if needed).
    fn read_cell_payload<'a>(
        &self,
        cx: &Cx,
        entry: &'a StackEntry,
        cell: &CellRef,
    ) -> Result<Cow<'a, [u8]>> {
        if cell.overflow_page.is_none() {
            return Ok(Cow::Borrowed(
                cell.local_payload(entry.page_data.as_bytes()),
            ));
        }

        instrumentation::record_owned_payload_materialization(
            usize::try_from(cell.payload_size).unwrap_or(usize::MAX),
        );
        let mut payload = Vec::new();
        self.read_cell_payload_into(cx, entry, cell, &mut payload)?;
        Ok(Cow::Owned(payload))
    }

    /// Binary search a leaf index page for a key.
    fn binary_search_index_leaf(
        &self,
        cx: &Cx,
        entry: &StackEntry,
        target: &[u8],
    ) -> Result<BinarySearchResult> {
        let _record_profile_scope = enter_record_profile_scope(RecordProfileScope::BtreeCursor);
        let count = entry.header.cell_count;
        if count == 0 {
            return Ok(BinarySearchResult::NotFound(0));
        }

        let mut lo = 0u16;
        let mut hi = count;
        let parsed_target = parse_record(target);

        while lo < hi {
            observe_cursor_cancellation(cx)?;
            let mid = lo + (hi - lo) / 2;
            let cell = self.parse_cell_at(entry, mid)?;
            let key = self.read_cell_payload(cx, entry, &cell)?;
            let ord = self.compare_index_key_bytes(key.as_ref(), target, parsed_target.as_deref());

            match ord {
                std::cmp::Ordering::Equal => return Ok(BinarySearchResult::Found(mid)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(BinarySearchResult::NotFound(lo))
    }

    /// Binary search an interior index page to find which child to descend.
    fn binary_search_index_interior(
        &self,
        cx: &Cx,
        entry: &StackEntry,
        target: &[u8],
    ) -> Result<BinarySearchResult> {
        let _record_profile_scope = enter_record_profile_scope(RecordProfileScope::BtreeCursor);
        let count = entry.header.cell_count;
        if count == 0 {
            return Ok(BinarySearchResult::NotFound(0));
        }

        let mut lo = 0u16;
        let mut hi = count;
        let parsed_target = parse_record(target);

        while lo < hi {
            observe_cursor_cancellation(cx)?;
            let mid = lo + (hi - lo) / 2;
            let cell = self.parse_cell_at(entry, mid)?;
            let key = self.read_cell_payload(cx, entry, &cell)?;
            let ord = self.compare_index_key_bytes(key.as_ref(), target, parsed_target.as_deref());

            // Note: target vs key comparison direction
            match ord {
                std::cmp::Ordering::Equal => return Ok(BinarySearchResult::Found(mid)),
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Less => lo = mid + 1,
            }
        }
        Ok(BinarySearchResult::NotFound(lo))
    }

    fn compare_index_key_bytes(
        &self,
        lhs_bytes: &[u8],
        rhs_bytes: &[u8],
        parsed_rhs: Option<&[fsqlite_types::SqliteValue]>,
    ) -> std::cmp::Ordering {
        let _record_profile_scope = enter_record_profile_scope(RecordProfileScope::BtreeCursor);
        match (parse_record(lhs_bytes), parsed_rhs) {
            (Some(lhs_vals), Some(rhs_vals)) => self
                .compare_index_key_values(&lhs_vals, rhs_vals)
                .unwrap_or_else(|| lhs_bytes.cmp(rhs_bytes)),
            _ => lhs_bytes.cmp(rhs_bytes),
        }
    }

    fn compare_index_key_values(
        &self,
        lhs: &[fsqlite_types::SqliteValue],
        rhs: &[fsqlite_types::SqliteValue],
    ) -> Option<std::cmp::Ordering> {
        let shared_len = lhs.len().min(rhs.len());
        for idx in 0..shared_len {
            let mut ord = lhs[idx].partial_cmp(&rhs[idx])?;
            if self.index_desc_flags.get(idx).copied().unwrap_or(false) {
                ord = ord.reverse();
            }
            if ord != std::cmp::Ordering::Equal {
                return Some(ord);
            }
        }
        Some(lhs.len().cmp(&rhs.len()))
    }

    /// Advance to the next entry. Returns false if at EOF.
    fn advance_next(&mut self, cx: &Cx) -> Result<bool> {
        observe_cursor_cancellation(cx)?;

        if self.at_eof {
            self.at_eof = true;
            return Ok(false);
        }
        if self.stack.is_empty() {
            // Allow recovering from a before-first state created by prev()
            // at the beginning of iteration.
            return self.move_to_leftmost_leaf(cx, self.root_page, true);
        }

        let depth = self.stack.len();
        let (is_leaf, cell_idx, cell_count) = {
            let top = &self.stack[depth - 1];
            (
                top.header.page_type.is_leaf(),
                top.cell_idx,
                top.header.cell_count,
            )
        };

        // On a leaf page: try to advance to the next cell.
        if is_leaf {
            let next_idx = cell_idx + 1;
            if next_idx < cell_count {
                self.stack[depth - 1].cell_idx = next_idx;
                return Ok(true);
            }

            // Past the last cell on this leaf. Pop up until we find an
            // interior page with more children to visit.
            self.stack.pop();
            while !self.stack.is_empty() {
                observe_cursor_cancellation(cx)?;
                let depth = self.stack.len();
                let parent = &self.stack[depth - 1];
                if parent.cell_idx < parent.header.cell_count {
                    // We came from the left child of cell[cell_idx].
                    // In index B-trees, the separator key in the parent is the next logical entry.
                    if !self.is_table {
                        return Ok(true);
                    }

                    let next_child_idx = parent.cell_idx + 1;
                    let child = self.child_page_at(parent, next_child_idx)?;

                    self.stack[depth - 1].cell_idx = next_child_idx;
                    self.issue_prefetch_hint(cx, child);
                    let found = self.move_to_leftmost_leaf(cx, child, true)?;
                    if found {
                        return Ok(true);
                    }
                    self.at_eof = false;
                    return self.advance_next(cx);
                }
                // cell_idx == cell_count means we already visited right_child.
                self.stack.pop();
            }

            // Exhausted the entire tree.
            self.at_eof = true;
            return Ok(false);
        }

        // On an interior page (only happens for index B-trees).
        // The current position is the separator cell itself.
        // The next logical entry is the leftmost descendant of the right subtree.
        let next_child_idx = cell_idx + 1;
        let child = {
            let top = &self.stack[depth - 1];
            self.child_page_at(top, next_child_idx)?
        };
        self.stack
            .last_mut()
            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?
            .cell_idx = next_child_idx;
        self.issue_prefetch_hint(cx, child);
        let found = self.move_to_leftmost_leaf(cx, child, false)?;
        if found {
            Ok(true)
        } else {
            self.at_eof = false;
            self.advance_next(cx)
        }
    }

    /// Move to the previous entry. Returns false if at the beginning.
    fn advance_prev(&mut self, cx: &Cx) -> Result<bool> {
        observe_cursor_cancellation(cx)?;

        if self.at_eof {
            // Recover from an after-last state (e.g., next() from last row).
            if self.stack.is_empty() {
                return self.move_to_rightmost_leaf(cx, self.root_page, true);
            }

            // Recover from a seek-past-end EOF sentinel while preserving leaf context.
            let (is_leaf, cell_count) = {
                let top = self
                    .stack
                    .last()
                    .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
                (top.header.page_type.is_leaf(), top.header.cell_count)
            };
            if is_leaf && cell_count > 0 {
                self.stack
                    .last_mut()
                    .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?
                    .cell_idx = cell_count - 1;
                self.at_eof = false;
                return Ok(true);
            }

            // Fallback to canonical rightmost positioning for any odd interior/empty state.
            return self.move_to_rightmost_leaf(cx, self.root_page, true);
        }

        if self.stack.is_empty() {
            return Ok(false);
        }

        let depth = self.stack.len();
        let (is_leaf, cell_idx) = {
            let top = &self.stack[depth - 1];
            (top.header.page_type.is_leaf(), top.cell_idx)
        };

        if is_leaf {
            if cell_idx > 0 {
                self.stack[depth - 1].cell_idx -= 1;
                self.at_eof = false;
                return Ok(true);
            }

            // Before the first cell on this leaf. Pop up.
            self.stack.pop();
            while !self.stack.is_empty() {
                observe_cursor_cancellation(cx)?;
                let depth = self.stack.len();
                let parent = &self.stack[depth - 1];
                if parent.cell_idx > 0 {
                    // In index B-trees, moving backward from a child should land on
                    // the separator key immediately to its left.
                    if !self.is_table {
                        self.stack[depth - 1].cell_idx -= 1;
                        return Ok(true);
                    }

                    let prev_child_idx = parent.cell_idx - 1;
                    let child = self.child_page_at(parent, prev_child_idx)?;

                    self.stack[depth - 1].cell_idx = prev_child_idx;
                    self.issue_prefetch_hint(cx, child);
                    let found = self.move_to_rightmost_leaf(cx, child, true)?;
                    if found {
                        return Ok(true);
                    }
                    self.at_eof = false;
                    return self.advance_prev(cx);
                }
                // cell_idx == 0 means we came from the leftmost child.
                self.stack.pop();
            }

            // At the very beginning of the tree.
            return Ok(false);
        }

        // On an interior page (only happens for index B-trees).
        // The current position is the separator cell itself.
        // The previous logical entry is the rightmost descendant of the left subtree.
        let child = {
            let top = &self.stack[depth - 1];
            self.child_page_at(top, cell_idx)?
        };
        self.issue_prefetch_hint(cx, child);
        let found = self.move_to_rightmost_leaf(cx, child, false)?;
        if found {
            Ok(true)
        } else {
            self.at_eof = false;
            self.advance_prev(cx)
        }
    }
}

#[allow(clippy::cast_possible_wrap)]
fn decode_big_endian_signed_fast(bytes: &[u8]) -> i64 {
    match bytes.len() {
        0 => 0,
        1 => bytes[0] as i8 as i64,
        2 => {
            let mut buf = [0_u8; 2];
            buf.copy_from_slice(bytes);
            i16::from_be_bytes(buf) as i64
        }
        3 => {
            let mut buf = [if bytes[0] & 0x80 != 0 { 0xFF } else { 0 }; 4];
            buf[1..4].copy_from_slice(bytes);
            i32::from_be_bytes(buf) as i64
        }
        4 => {
            let mut buf = [0_u8; 4];
            buf.copy_from_slice(bytes);
            i32::from_be_bytes(buf) as i64
        }
        6 => {
            let mut buf = [if bytes[0] & 0x80 != 0 { 0xFF } else { 0 }; 8];
            buf[2..8].copy_from_slice(bytes);
            i64::from_be_bytes(buf)
        }
        8 => {
            let mut buf = [0_u8; 8];
            buf.copy_from_slice(bytes);
            i64::from_be_bytes(buf)
        }
        _ => {
            let negative = bytes.first().is_some_and(|&b| b & 0x80 != 0);
            let mut value: u64 = if negative { u64::MAX } else { 0 };
            for &b in bytes {
                value = (value << 8) | u64::from(b);
            }
            value as i64
        }
    }
}

/// Result of a binary search within a page.
enum BinarySearchResult {
    /// Exact match found at this cell index.
    Found(u16),
    /// No match; the target would be inserted at this position.
    NotFound(u16),
}

impl<P: PageWriter> BtCursor<P> {
    fn write_overflow_chain_for_insert(
        &mut self,
        cx: &Cx,
        overflow_data: &[u8],
    ) -> Result<PageNumber> {
        if overflow_data.is_empty() {
            return Err(FrankenError::internal(
                "overflow writer called with empty payload",
            ));
        }
        if self.usable_size <= 4 {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "invalid usable page size {} for overflow chain",
                    self.usable_size
                ),
            });
        }

        #[allow(clippy::cast_possible_truncation)]
        let bytes_per_page = self.usable_size.saturating_sub(4) as usize;
        if bytes_per_page == 0 {
            return Err(FrankenError::DatabaseCorrupt {
                detail: "usable page size too small for overflow pages".to_owned(),
            });
        }
        #[allow(clippy::cast_possible_truncation)]
        let page_size = self.page_size as usize;
        let num_pages = overflow_data.len().div_ceil(bytes_per_page);
        if num_pages > overflow::MAX_OVERFLOW_CHAIN {
            return Err(FrankenError::TooBig);
        }

        let mut pages = Vec::with_capacity(num_pages);
        for _ in 0..num_pages {
            match self.pager.allocate_page(cx) {
                Ok(pgno) => pages.push(pgno),
                Err(err) => {
                    for leaked in pages {
                        let _ = self.pager.free_page(cx, leaked);
                    }
                    return Err(err);
                }
            }
        }

        let mut page_buf = vec![0u8; page_size];
        for (idx, &pgno) in pages.iter().enumerate() {
            let data_start = idx * bytes_per_page;
            let data_end = ((idx + 1) * bytes_per_page).min(overflow_data.len());
            let chunk = &overflow_data[data_start..data_end];

            let next = if idx + 1 < pages.len() {
                pages[idx + 1].get()
            } else {
                0
            };

            page_buf[0..4].copy_from_slice(&next.to_be_bytes());
            page_buf[4..4 + chunk.len()].copy_from_slice(chunk);
            if chunk.len() < bytes_per_page {
                // Ensure tail is zeroed if the chunk didn't fill the space.
                page_buf[4 + chunk.len()..].fill(0);
            }
            let owned_page = std::mem::replace(&mut page_buf, vec![0_u8; page_size]);
            if let Err(err) = self
                .pager
                .write_page_data(cx, pgno, PageData::from_vec(owned_page))
            {
                // Best-effort cleanup: any overflow pages allocated for this
                // cell must be released if chain materialization fails midway.
                for leaked in pages.iter().copied() {
                    let _ = self.pager.free_page(cx, leaked);
                }
                return Err(err);
            }
        }

        Ok(pages[0])
    }

    fn free_overflow_chain(&mut self, cx: &Cx, first: PageNumber) -> Result<()> {
        let _mask = cx.masked();
        let mut current = Some(first);
        let mut visited = 0usize;

        while let Some(pgno) = current {
            // Once overflow cleanup starts, finish the chain so the statement
            // cannot strand partially-freed pages behind an interrupt.
            visited += 1;
            if visited > overflow::MAX_OVERFLOW_CHAIN {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "overflow chain exceeds {} pages while freeing",
                        overflow::MAX_OVERFLOW_CHAIN
                    ),
                });
            }

            let page = self.pager.read_page_data(cx, pgno)?;
            let page_bytes = page.as_bytes();
            if page_bytes.len() < 4 {
                warn!(
                    page = pgno.get(),
                    page_len = page_bytes.len(),
                    "overflow chain corruption detected while freeing"
                );
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("overflow page {} too small while freeing", pgno.get()),
                });
            }

            let next =
                u32::from_be_bytes([page_bytes[0], page_bytes[1], page_bytes[2], page_bytes[3]]);
            current = PageNumber::new(next);
            self.pager.free_page(cx, pgno)?;
        }

        Ok(())
    }

    /// Encode a table leaf cell into the provided buffer, returning the
    /// overflow head page (if any).
    ///
    /// The buffer is cleared and reused across calls so repeated inserts
    /// reuse the same heap allocation.
    #[inline]
    fn encode_table_leaf_cell_into(
        &mut self,
        cx: &Cx,
        rowid: i64,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<Option<PageNumber>> {
        out.clear();
        let payload_size = u32::try_from(payload.len()).map_err(|_| FrankenError::TooBig)?;
        let payload_size_u64 = u64::from(payload_size);
        let local_size = cell::local_payload_size(
            payload_size,
            self.usable_size,
            cell::BtreePageType::LeafTable,
        ) as usize;
        let local_size = local_size.min(payload.len());

        let mut varint = [0u8; 9];
        let p_len = write_varint(&mut varint, payload_size_u64);
        out.extend_from_slice(&varint[..p_len]);

        let rowid_bits = u64::from_ne_bytes(rowid.to_ne_bytes());
        let r_len = write_varint(&mut varint, rowid_bits);
        out.extend_from_slice(&varint[..r_len]);
        out.extend_from_slice(&payload[..local_size]);

        if local_size < payload.len() {
            let first_overflow =
                self.write_overflow_chain_for_insert(cx, &payload[local_size..])?;
            out.extend_from_slice(&first_overflow.get().to_be_bytes());
            instrumentation::record_table_leaf_cell_assembly(out.len());
            Ok(Some(first_overflow))
        } else {
            instrumentation::record_table_leaf_cell_assembly(out.len());
            Ok(None)
        }
    }

    /// Backwards-compatible wrapper — allocates a fresh Vec per call.
    #[allow(dead_code)]
    fn encode_table_leaf_cell(
        &mut self,
        cx: &Cx,
        rowid: i64,
        payload: &[u8],
    ) -> Result<(Vec<u8>, Option<PageNumber>)> {
        let mut out = Vec::with_capacity(24 + payload.len().min(self.usable_size as usize) + 4);
        let overflow = self.encode_table_leaf_cell_into(cx, rowid, payload, &mut out)?;
        Ok((out, overflow))
    }

    /// Encode an index leaf cell into the provided buffer, returning the
    /// overflow head page (if any).
    fn encode_index_leaf_cell_into(
        &mut self,
        cx: &Cx,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<Option<PageNumber>> {
        out.clear();
        let payload_size = u32::try_from(key.len()).map_err(|_| FrankenError::TooBig)?;
        let payload_size_u64 = u64::from(payload_size);
        let local_size = cell::local_payload_size(
            payload_size,
            self.usable_size,
            cell::BtreePageType::LeafIndex,
        ) as usize;
        let local_size = local_size.min(key.len());

        let mut varint = [0u8; 9];
        let p_len = write_varint(&mut varint, payload_size_u64);
        out.extend_from_slice(&varint[..p_len]);
        out.extend_from_slice(&key[..local_size]);

        if local_size < key.len() {
            let first_overflow = self.write_overflow_chain_for_insert(cx, &key[local_size..])?;
            out.extend_from_slice(&first_overflow.get().to_be_bytes());
            instrumentation::record_index_leaf_cell_assembly(out.len());
            Ok(Some(first_overflow))
        } else {
            instrumentation::record_index_leaf_cell_assembly(out.len());
            Ok(None)
        }
    }

    /// Backwards-compatible wrapper — allocates a fresh Vec per call.
    #[allow(dead_code)]
    fn encode_index_leaf_cell(
        &mut self,
        cx: &Cx,
        key: &[u8],
    ) -> Result<(Vec<u8>, Option<PageNumber>)> {
        let mut out = Vec::with_capacity(16 + key.len().min(self.usable_size as usize) + 4);
        let overflow = self.encode_index_leaf_cell_into(cx, key, &mut out)?;
        Ok((out, overflow))
    }

    /// Try to insert a cell directly onto the leaf page at the top of the
    /// cursor stack. Returns `Ok(true)` if the cell was inserted, or
    /// `Ok(false)` if the page is full and balance is needed.
    fn try_insert_on_leaf(&mut self, cx: &Cx, insert_idx: u16, cell_data: &[u8]) -> Result<bool> {
        let depth = self.stack.len();
        if depth == 0 {
            return Err(FrankenError::internal("cursor stack is empty"));
        }
        let leaf_page_no = self.stack[depth - 1].page_no;

        let mut page_data = self.pager.read_page_data(cx, leaf_page_no)?;
        let header_offset = cell::header_offset_for_page(leaf_page_no);
        let mut header = cell::parse_page_header(page_data.as_bytes(), leaf_page_no)?;
        let mut ptrs = cell::read_cell_pointers(page_data.as_bytes(), &header, header_offset)?;

        let content_offset = header.content_offset(self.usable_size);
        let Some(new_content_offset) = content_offset.checked_sub(cell_data.len()) else {
            return Ok(false); // Page full.
        };

        let ptr_array_end = header_offset
            + usize::from(header.page_type.header_size())
            + (usize::from(header.cell_count) + 1) * 2;
        if ptr_array_end > new_content_offset {
            return Ok(false); // Page full.
        }

        let insert_at = usize::from(insert_idx).min(ptrs.len());
        #[allow(clippy::cast_possible_truncation)]
        {
            ptrs.insert(insert_at, new_content_offset as u16);
            header.cell_count = ptrs.len() as u16;
            header.cell_content_offset = new_content_offset as u32;
        }
        {
            let page_bytes = page_data.as_bytes_mut();
            page_bytes[new_content_offset..new_content_offset + cell_data.len()]
                .copy_from_slice(cell_data);
            header.write(page_bytes, header_offset);
            cell::write_cell_pointers(page_bytes, header_offset, &header, &ptrs);
        }

        self.pager.write_page_data(cx, leaf_page_no, page_data)?;

        // Refresh the top stack entry from the pager.
        let mut refreshed = self.reload_page_fresh(cx, leaf_page_no)?;
        #[allow(clippy::cast_possible_truncation)]
        {
            refreshed.cell_idx = insert_at as u16;
        }
        self.stack[depth - 1] = refreshed;
        self.at_eof = false;
        Ok(true)
    }

    /// Try to append a cell onto the current leaf page already loaded at the
    /// top of the cursor stack.
    ///
    /// This is used by the rightmost-leaf hint path, where the leaf was just
    /// loaded and validated in the same function. That lets us reuse the
    /// cached page/header directly and avoid a second pager read plus a full
    /// cell-pointer-array decode/reload cycle on the hot append case.
    fn try_append_on_current_leaf(&mut self, cx: &Cx, cell_data: &[u8]) -> Result<bool> {
        let mut entry = self
            .stack
            .pop()
            .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
        let leaf_page_no = entry.page_no;
        let header_offset = cell::header_offset_for_page(leaf_page_no);
        let insert_idx = entry.header.cell_count;
        let content_offset = entry.header.content_offset(self.usable_size);
        let Some(new_content_offset) = content_offset.checked_sub(cell_data.len()) else {
            self.stack.push(entry);
            return Ok(false);
        };

        let ptr_array_end = header_offset
            + usize::from(entry.header.page_type.header_size())
            + (usize::from(insert_idx) + 1) * 2;
        if ptr_array_end > new_content_offset {
            self.stack.push(entry);
            return Ok(false);
        }

        let new_cell_offset =
            u16::try_from(new_content_offset).map_err(|_| FrankenError::DatabaseCorrupt {
                detail: format!(
                    "new leaf cell offset {} exceeds u16 range on page {}",
                    new_content_offset,
                    leaf_page_no.get()
                ),
            })?;
        let new_cell_count =
            insert_idx
                .checked_add(1)
                .ok_or_else(|| FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "leaf page {} cell count overflow while appending",
                        leaf_page_no.get()
                    ),
                })?;
        let ptr_offset = header_offset
            + usize::from(entry.header.page_type.header_size())
            + usize::from(insert_idx) * 2;

        entry.header.cell_count = new_cell_count;
        entry.header.cell_content_offset =
            u32::try_from(new_content_offset).map_err(|_| FrankenError::DatabaseCorrupt {
                detail: format!(
                    "new leaf content offset {} exceeds u32 range on page {}",
                    new_content_offset,
                    leaf_page_no.get()
                ),
            })?;
        {
            let page_bytes = entry.page_data.as_bytes_mut();
            page_bytes[new_content_offset..new_content_offset + cell_data.len()]
                .copy_from_slice(cell_data);
            page_bytes[ptr_offset..ptr_offset + 2].copy_from_slice(&new_cell_offset.to_be_bytes());
            entry.header.write(page_bytes, header_offset);
        }

        let staged_page = entry.page_data.clone();
        self.pager.write_page_data(cx, leaf_page_no, staged_page)?;

        let mut updated_cell_pointers = Vec::with_capacity(entry.cell_pointers.len() + 1);
        updated_cell_pointers.extend_from_slice(&entry.cell_pointers);
        updated_cell_pointers.push(new_cell_offset);
        entry.cell_pointers = Arc::<[u16]>::from(updated_cell_pointers);
        entry.cell_idx = insert_idx;

        self.stack.push(entry);
        self.at_eof = false;
        Ok(true)
    }

    /// Balance the tree after an insert when the leaf page is full.
    ///
    /// `cell_data` is the cell that didn't fit. `insert_idx` is where within
    /// the leaf's cell array it should be placed.
    fn balance_for_insert(&mut self, cx: &Cx, cell_data: &[u8], insert_idx: u16) -> Result<()> {
        let depth = self.stack.len();
        if depth == 0 {
            return Err(FrankenError::internal("cursor stack empty during balance"));
        }

        if depth == 1 {
            // Leaf is the root — push root down first.
            balance::balance_deeper(
                cx,
                &mut self.pager,
                self.root_page,
                self.usable_size,
                self.page_size,
            )?;
            self.note_split_event();
            // Root is now an interior page with 1 child at index 0.
            let outcome = balance::balance_nonroot(
                cx,
                &mut self.pager,
                self.root_page,
                0,
                &[cell_data.to_vec()],
                insert_idx as usize,
                self.usable_size,
                self.page_size,
                true,
            )?;
            if matches!(outcome, balance::BalanceResult::Split { .. }) {
                return Err(FrankenError::internal(
                    "root balance unexpectedly returned split requiring parent update",
                ));
            }
        } else {
            let parent_page_no = self.stack[depth - 2].page_no;
            let child_idx = self.stack[depth - 2].cell_idx as usize;
            let parent_is_root = parent_page_no == self.root_page;

            // Attempt balance_quick optimization for sequential inserts.
            // This avoids full 3-sibling rewrites when just appending to the right.
            let leaf_entry = &self.stack[depth - 1];
            let parent_entry = &self.stack[depth - 2];

            if leaf_entry.header.page_type == cell::BtreePageType::LeafTable
                && insert_idx == leaf_entry.header.cell_count
                && child_idx == parent_entry.header.cell_count as usize
            {
                if let Some((_, n)) = read_varint(cell_data) {
                    if let Some((rowid, _)) = read_varint(&cell_data[n..]) {
                        #[allow(clippy::cast_possible_wrap)]
                        let rowid = rowid as i64;
                        match balance::balance_quick(
                            cx,
                            &mut self.pager,
                            parent_page_no,
                            leaf_entry.page_no,
                            cell_data,
                            rowid,
                            self.usable_size,
                            self.page_size,
                        ) {
                            Ok(Some(_new_pgno)) => {
                                self.note_split_event();
                                // Invalidate cursor stack as tree structure changed.
                                self.stack.clear();
                                self.at_eof = true;
                                return Ok(());
                            }
                            Ok(None) => {}
                            Err(err) => return Err(err),
                        }
                    }
                }
            }

            let mut outcome = if leaf_entry.header.page_type == cell::BtreePageType::LeafTable {
                match balance::balance_table_leaf_local_split(
                    cx,
                    &mut self.pager,
                    parent_page_no,
                    child_idx,
                    leaf_entry.page_no,
                    cell_data,
                    insert_idx as usize,
                    self.usable_size,
                    self.page_size,
                    parent_is_root,
                )? {
                    Some(outcome) => {
                        self.note_split_event();
                        outcome
                    }
                    None => balance::balance_nonroot(
                        cx,
                        &mut self.pager,
                        parent_page_no,
                        child_idx,
                        &[cell_data.to_vec()],
                        insert_idx as usize,
                        self.usable_size,
                        self.page_size,
                        parent_is_root,
                    )?,
                }
            } else {
                balance::balance_nonroot(
                    cx,
                    &mut self.pager,
                    parent_page_no,
                    child_idx,
                    &[cell_data.to_vec()],
                    insert_idx as usize,
                    self.usable_size,
                    self.page_size,
                    parent_is_root,
                )?
            };

            // If balancing split the parent page, propagate the split up the
            // cursor stack by updating each ancestor in turn.
            let mut parent_level = depth - 2; // stack index of the split page
            let mut split_page_no = parent_page_no;
            while let balance::BalanceResult::Split {
                new_pgnos,
                new_dividers,
            } = outcome
            {
                // Once we start rewriting parent links, finish propagating the
                // split so we do not return with a half-rebalanced tree.
                self.note_split_event();
                if parent_level == 0 {
                    return Err(FrankenError::internal(
                        "balance split bubbled above root (unexpected)",
                    ));
                }

                let ancestor_page_no = self.stack[parent_level - 1].page_no;
                let ancestor_child_idx = usize::from(self.find_child_slot_by_page_no(
                    cx,
                    ancestor_page_no,
                    split_page_no,
                )?);
                let ancestor_is_root = ancestor_page_no == self.root_page;

                outcome = balance::apply_child_replacement(
                    cx,
                    &mut self.pager,
                    ancestor_page_no,
                    self.usable_size,
                    self.page_size,
                    ancestor_child_idx,
                    1, // Replacing a single child page with its split siblings.
                    &new_pgnos,
                    &new_dividers,
                    ancestor_is_root,
                )?;

                split_page_no = ancestor_page_no;
                parent_level -= 1;
            }
        }

        // Tree structure changed — invalidate the cursor stack.
        self.stack.clear();
        self.at_eof = true;
        Ok(())
    }

    /// Balance the tree after deleting from a non-root leaf.
    ///
    /// For a root leaf page (single-level tree), no balancing is required.
    ///
    /// This propagates the rebalance upward through the tree.  When
    /// merging siblings at one level reduces the parent to zero cells,
    /// the parent itself becomes a candidate for merging with *its*
    /// siblings at the next level up.  At the root level,
    /// `apply_child_replacement` triggers `balance_shallower` which
    /// copies the sole remaining child into the root page, reducing the
    /// tree depth by one (the inverse of `balance_deeper`).
    fn balance_for_delete(&mut self, cx: &Cx) -> Result<()> {
        let depth = self.stack.len();
        if depth <= 1 {
            return Ok(());
        }

        // Start at the leaf's parent and propagate upward as needed.
        let mut level = depth - 2;

        loop {
            // Deletion has already mutated the tree by the time rebalance
            // begins, so we intentionally finish this fixup even if the
            // caller's context becomes cancelled mid-flight.
            let parent_page_no = self.stack[level].page_no;
            let child_idx = usize::from(self.stack[level].cell_idx);
            let parent_is_root = parent_page_no == self.root_page;

            self.note_merge_event();
            let mut outcome = balance::balance_nonroot(
                cx,
                &mut self.pager,
                parent_page_no,
                child_idx,
                &[],
                0,
                self.usable_size,
                self.page_size,
                parent_is_root,
            )?;

            // If balancing split the parent page, propagate the split up the
            // cursor stack by updating each ancestor in turn.
            let mut split_level = level;
            let mut split_page_no = parent_page_no;
            while let balance::BalanceResult::Split {
                new_pgnos,
                new_dividers,
            } = outcome
            {
                // Keep split propagation atomic with respect to interrupts for
                // the same reason as balance_for_insert above.
                self.note_split_event();
                if split_level == 0 {
                    return Err(FrankenError::internal(
                        "balance split bubbled above root (unexpected)",
                    ));
                }

                let ancestor_page_no = self.stack[split_level - 1].page_no;
                let ancestor_child_idx = usize::from(self.find_child_slot_by_page_no(
                    cx,
                    ancestor_page_no,
                    split_page_no,
                )?);
                let ancestor_is_root = ancestor_page_no == self.root_page;

                outcome = balance::apply_child_replacement(
                    cx,
                    &mut self.pager,
                    ancestor_page_no,
                    self.usable_size,
                    self.page_size,
                    ancestor_child_idx,
                    1, // Replacing a single child page with its split siblings.
                    &new_pgnos,
                    &new_dividers,
                    ancestor_is_root,
                )?;

                split_page_no = ancestor_page_no;
                split_level -= 1;
            }

            // If we just balanced at the root level, we are done.
            // balance_shallower (called from apply_child_replacement)
            // already handled the 0-cell root case.
            if parent_is_root || level == 0 {
                break;
            }

            // Check whether the parent now has zero cells — if so, it
            // needs to be merged with its siblings at the next level up.
            let parent_data = self.pager.read_page_data(cx, parent_page_no)?;
            let parent_header = cell::parse_page_header(parent_data.as_bytes(), parent_page_no)?;

            if parent_header.cell_count == 0 && parent_header.page_type.is_interior() {
                level -= 1;
            } else {
                break;
            }
        }

        // Tree shape may change after balancing.
        self.stack.clear();
        self.at_eof = true;
        Ok(())
    }

    fn replace_interior_cell(&mut self, cx: &Cx, new_payload: &[u8]) -> Result<bool> {
        if self.stack.is_empty() || self.at_eof {
            return Err(FrankenError::internal(
                "cursor at EOF during interior replace",
            ));
        }
        let top =
            self.stack.last().cloned().ok_or_else(|| {
                FrankenError::internal("cursor stack empty during interior replace")
            })?;
        let page_no = top.page_no;
        let cell_idx = top.cell_idx;

        let cell_ref = self.parse_cell_at(&top, cell_idx)?;
        let left_child = cell_ref
            .left_child
            .ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: "interior cell missing left child pointer".to_owned(),
            })?;

        // Encode the new cell.
        let mut new_cell = Vec::new();
        new_cell.extend_from_slice(&left_child.get().to_be_bytes());

        let payload_size = u32::try_from(new_payload.len()).map_err(|_| FrankenError::TooBig)?;
        let mut varint = [0u8; 9];
        let p_len = write_varint(&mut varint, u64::from(payload_size));
        new_cell.extend_from_slice(&varint[..p_len]);

        let local_size =
            cell::local_payload_size(payload_size, self.usable_size, top.header.page_type) as usize;
        let local_size = local_size.min(new_payload.len());

        new_cell.extend_from_slice(&new_payload[..local_size]);
        let new_overflow_head = if local_size < new_payload.len() {
            let first_overflow =
                self.write_overflow_chain_for_insert(cx, &new_payload[local_size..])?;
            new_cell.extend_from_slice(&first_overflow.get().to_be_bytes());
            Some(first_overflow)
        } else {
            None
        };
        instrumentation::record_interior_cell_rebuild(new_cell.len());

        // Remove old cell from page and try to insert new cell.
        let mut page_data = self.pager.read_page_data(cx, page_no)?;
        let header_offset = cell::header_offset_for_page(page_no);
        let mut header = cell::parse_page_header(page_data.as_bytes(), page_no)?;
        let mut ptrs = cell::read_cell_pointers(page_data.as_bytes(), &header, header_offset)?;
        let cell_idx_usize = usize::from(cell_idx);
        if cell_idx_usize >= ptrs.len() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "interior replace index {} out of bounds for page {} with {} cells",
                    cell_idx,
                    page_no,
                    ptrs.len()
                ),
            });
        }

        let old_overflow = cell_ref.overflow_page;
        ptrs.remove(cell_idx_usize);

        // Defragment.
        let mut new_content_offset = self.usable_size as usize;
        let ptr_array_end =
            header_offset + usize::from(header.page_type.header_size()) + ptrs.len() * 2;

        let mut cells_to_move = Vec::with_capacity(ptrs.len());
        for (i, &off) in ptrs.iter().enumerate() {
            let ptr = off as usize;
            let cell = CellRef::parse(
                page_data.as_bytes(),
                ptr,
                header.page_type,
                self.usable_size,
            )?;
            // Full on-page size: header varints (payload_offset - ptr) + local payload + overflow ptr.
            let size = crate::payload::cell_on_page_size(&cell, ptr);
            cells_to_move.push((ptr, size, i));
        }

        // Sort by ptr descending so we can shift right safely without overwriting unread data.
        cells_to_move.sort_unstable_by_key(|k| std::cmp::Reverse(k.0));

        for (ptr, size, i) in cells_to_move {
            new_content_offset = new_content_offset.checked_sub(size).ok_or_else(|| {
                FrankenError::DatabaseCorrupt {
                    detail: "cell size overflow during interior defragmentation".to_owned(),
                }
            })?;
            if new_content_offset < ptr_array_end {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: "cell content overlaps pointer array during defragmentation".to_owned(),
                });
            }
            page_data
                .as_bytes_mut()
                .copy_within(ptr..ptr + size, new_content_offset);
            ptrs[i] = new_content_offset as u16;
        }

        // Check if new cell fits.
        let ptr_array_end_with_new = ptr_array_end + 2;
        let fits = ptr_array_end_with_new
            .checked_add(new_cell.len())
            .is_some_and(|needed| new_content_offset >= needed);
        if fits {
            new_content_offset -= new_cell.len();
            let new_end = new_content_offset + new_cell.len();
            ptrs.insert(
                cell_idx_usize,
                u16::try_from(new_content_offset).map_err(|_| FrankenError::DatabaseCorrupt {
                    detail: "new interior cell offset exceeds u16 range".to_owned(),
                })?,
            );

            header.cell_count =
                u16::try_from(ptrs.len()).map_err(|_| FrankenError::DatabaseCorrupt {
                    detail: "interior cell count exceeds u16 range".to_owned(),
                })?;
            header.cell_content_offset =
                u32::try_from(new_content_offset).map_err(|_| FrankenError::DatabaseCorrupt {
                    detail: "interior content offset exceeds u32 range".to_owned(),
                })?;
            {
                let page_bytes = page_data.as_bytes_mut();
                page_bytes[new_content_offset..new_end].copy_from_slice(&new_cell);
                header.write(page_bytes, header_offset);
                cell::write_cell_pointers(page_bytes, header_offset, &header, &ptrs);
            }

            self.pager.write_page_data(cx, page_no, page_data)?;
            let mut refreshed = self.reload_page_fresh(cx, page_no)?;
            refreshed.cell_idx = cell_idx;
            if let Some(top) = self.stack.last_mut() {
                *top = refreshed;
            }
            self.at_eof = false;
            if let Some(first) = old_overflow {
                self.free_overflow_chain(cx, first)?;
            }
            return Ok(false);
        }

        // It doesn't fit! We must delete the old cell and balance.
        header.cell_count =
            u16::try_from(ptrs.len()).map_err(|_| FrankenError::DatabaseCorrupt {
                detail: "interior cell count exceeds u16 range".to_owned(),
            })?;
        header.cell_content_offset =
            u32::try_from(new_content_offset).map_err(|_| FrankenError::DatabaseCorrupt {
                detail: "interior content offset exceeds u32 range".to_owned(),
            })?;
        {
            let page_bytes = page_data.as_bytes_mut();
            header.write(page_bytes, header_offset);
            cell::write_cell_pointers(page_bytes, header_offset, &header, &ptrs);
        }

        self.pager.write_page_data(cx, page_no, page_data)?;

        // Free the OLD overflow chain since we are discarding the old cell.
        if let Some(first) = old_overflow {
            self.free_overflow_chain(cx, first)?;
        }

        // Now we must insert `new_cell` at `cell_idx`, which will trigger a structural rebalance.
        let balance_result = self.balance_for_insert(cx, &new_cell, cell_idx);
        if balance_result.is_err() {
            if let Some(first) = new_overflow_head {
                let _ = self.free_overflow_chain(cx, first);
            }
        }
        balance_result?;

        Ok(true)
    }

    fn remove_cell_from_leaf(&mut self, cx: &Cx) -> Result<(PageNumber, u16)> {
        let depth = self.stack.len();
        if depth == 0 || self.at_eof {
            return Err(FrankenError::internal("cursor at EOF during remove"));
        }

        let (overflow_head, delete_idx, leaf_page_no) = {
            let top = &self.stack[depth - 1];
            if !top.header.page_type.is_leaf() {
                return Err(FrankenError::internal(
                    "remove_cell_from_leaf called on interior page",
                ));
            }

            let delete_idx = usize::from(top.cell_idx);
            if delete_idx >= top.cell_pointers.len() {
                return Err(FrankenError::internal("cursor position out of bounds"));
            }

            let cell_ref = self.parse_cell_at(top, top.cell_idx)?;
            (cell_ref.overflow_page, delete_idx, top.page_no)
        };

        // Identify overflow chain to free, but DO NOT free it yet.
        // We must remove the pointer from the leaf page first. If we freed
        // the chain first and then failed to update the leaf, the leaf would
        // contain a dangling pointer to a freed (and potentially reused) page,
        // causing corruption.
        //
        // If we update the leaf first and then fail to free the chain, we leak
        // pages but preserve database integrity. Leaks are recoverable (VACUUM);
        // corruption is not.
        let mut page_data = self.pager.read_page_data(cx, leaf_page_no)?;
        let header_offset = cell::header_offset_for_page(leaf_page_no);
        let mut header = cell::parse_page_header(page_data.as_bytes(), leaf_page_no)?;
        let mut ptrs = cell::read_cell_pointers(page_data.as_bytes(), &header, header_offset)?;
        if delete_idx >= ptrs.len() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "delete_idx {} out of bounds for page {} with {} cells",
                    delete_idx,
                    leaf_page_no,
                    ptrs.len()
                ),
            });
        }
        ptrs.remove(delete_idx);

        // Defragment the page to reclaim the space used by the deleted cell.
        // This avoids maintaining a complex freeblock list and keeps fragmented_free_bytes at 0.
        let ptr_array_end =
            header_offset + usize::from(header.page_type.header_size()) + ptrs.len() * 2;

        let mut cells_to_move = Vec::with_capacity(ptrs.len());
        for (i, &off) in ptrs.iter().enumerate() {
            let ptr = off as usize;
            let cell = CellRef::parse(
                page_data.as_bytes(),
                ptr,
                header.page_type,
                self.usable_size,
            )?;
            // Full on-page size: header varints (payload_offset - ptr) + local payload + overflow ptr.
            let size = crate::payload::cell_on_page_size(&cell, ptr);
            cells_to_move.push((ptr, size, i));
        }

        // Sort by ptr descending so we can shift right safely without overwriting unread data.
        cells_to_move.sort_unstable_by_key(|k| std::cmp::Reverse(k.0));

        let mut new_content_offset = self.usable_size as usize;
        for (ptr, size, i) in cells_to_move {
            new_content_offset = new_content_offset.checked_sub(size).ok_or_else(|| {
                FrankenError::DatabaseCorrupt {
                    detail: "cell size overflow during defragmentation".to_owned(),
                }
            })?;
            if new_content_offset < ptr_array_end {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: "cell content overlaps pointer array during defragmentation".to_owned(),
                });
            }
            page_data
                .as_bytes_mut()
                .copy_within(ptr..ptr + size, new_content_offset);
            ptrs[i] = new_content_offset as u16;
        }

        // Fill the now-unused space with zeros for cleanliness (optional, but good for reproducibility/debugging).
        if new_content_offset > ptr_array_end {
            page_data.as_bytes_mut()[ptr_array_end..new_content_offset].fill(0);
        }

        #[allow(clippy::cast_possible_truncation)]
        {
            header.cell_count = ptrs.len() as u16;
            header.cell_content_offset = new_content_offset as u32;
        }
        header.fragmented_free_bytes = 0;
        header.first_freeblock = 0;

        {
            let page_bytes = page_data.as_bytes_mut();
            header.write(page_bytes, header_offset);
            cell::write_cell_pointers(page_bytes, header_offset, &header, &ptrs);
        }
        self.pager.write_page_data(cx, leaf_page_no, page_data)?;

        // Refresh the stack entry.
        let mut refreshed = self.reload_page_fresh(cx, leaf_page_no)?;
        let new_count = refreshed.header.cell_count;
        if new_count == 0 {
            refreshed.cell_idx = 0;
            self.at_eof = true;
            self.stack[depth - 1] = refreshed;
        } else if delete_idx >= usize::from(new_count) {
            refreshed.cell_idx = new_count - 1;
            self.at_eof = false;
            self.stack[depth - 1] = refreshed;
            self.advance_next(cx)?;
        } else {
            #[allow(clippy::cast_possible_truncation)]
            {
                refreshed.cell_idx = delete_idx as u16;
            }
            self.at_eof = false;
            self.stack[depth - 1] = refreshed;
        }

        // Now it is safe to free the overflow chain.
        if let Some(first) = overflow_head {
            self.free_overflow_chain(cx, first)?;
        }

        Ok((leaf_page_no, new_count))
    }

    fn table_insert_from_current_position(
        &mut self,
        cx: &Cx,
        rowid: i64,
        data: &[u8],
    ) -> Result<()> {
        let insert_idx = {
            let top = self
                .stack
                .last()
                .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
            if self.at_eof {
                top.header.cell_count
            } else {
                top.cell_idx
            }
        };

        // Take cell_buf for reuse so repeated inserts preserve allocation capacity.
        let mut cell_data = std::mem::take(&mut self.cell_buf);
        let overflow_head = self.encode_table_leaf_cell_into(cx, rowid, data, &mut cell_data)?;

        match self.try_insert_on_leaf(cx, insert_idx, &cell_data) {
            Ok(true) => {
                self.cell_buf = cell_data;
                self.last_insert_rowid = Some(rowid);
                Ok(())
            }
            Ok(false) => {
                // Page full — balance and redistribute.
                let balance_result = self.balance_for_insert(cx, &cell_data, insert_idx);
                self.cell_buf = cell_data;
                if balance_result.is_ok() {
                    self.last_insert_rowid = Some(rowid);
                } else if let Some(first) = overflow_head {
                    let _ = self.free_overflow_chain(cx, first);
                }
                balance_result
            }
            Err(error) => {
                self.cell_buf = cell_data;
                if let Some(first) = overflow_head {
                    let _ = self.free_overflow_chain(cx, first);
                }
                Err(error)
            }
        }
    }

    fn index_insert_from_current_position(&mut self, cx: &Cx, key: &[u8]) -> Result<()> {
        let insert_idx = {
            let top = self
                .stack
                .last()
                .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
            if !top.header.page_type.is_leaf() {
                return Err(FrankenError::internal(
                    "index insert from current position requires leaf cursor state",
                ));
            }
            if self.at_eof {
                top.header.cell_count
            } else {
                top.cell_idx
            }
        };

        let mut cell_data = std::mem::take(&mut self.cell_buf);
        let overflow_head = self.encode_index_leaf_cell_into(cx, key, &mut cell_data)?;

        match self.try_insert_on_leaf(cx, insert_idx, &cell_data) {
            Ok(true) => {
                self.cell_buf = cell_data;
                Ok(())
            }
            Ok(false) => {
                let balance_result = self.balance_for_insert(cx, &cell_data, insert_idx);
                self.cell_buf = cell_data;
                if balance_result.is_err() {
                    if let Some(first) = overflow_head {
                        let _ = self.free_overflow_chain(cx, first);
                    }
                }
                balance_result
            }
            Err(error) => {
                self.cell_buf = cell_data;
                if let Some(first) = overflow_head {
                    let _ = self.free_overflow_chain(cx, first);
                }
                Err(error)
            }
        }
    }

    fn try_table_append_on_hinted_leaf(
        &mut self,
        cx: &Cx,
        hinted_leaf_page: PageNumber,
        rowid: i64,
        data: &[u8],
    ) -> Result<bool> {
        observe_cursor_cancellation(cx)?;
        self.stack.clear();
        self.at_eof = true;

        let mut entry = match self.load_page(cx, hinted_leaf_page) {
            Ok(entry) => entry,
            Err(_) => return Ok(false),
        };
        if entry.header.page_type != cell::BtreePageType::LeafTable || entry.header.cell_count == 0
        {
            self.stack.clear();
            self.at_eof = true;
            return Ok(false);
        }

        entry.cell_idx = entry.header.cell_count - 1;
        self.stack.push(entry);
        self.at_eof = false;
        self.record_range_page_witness(hinted_leaf_page);

        // bd-wwqen.3: use cached rowid if available for this leaf page,
        // avoiding a cell parse just to read the last rowid.
        let last_rowid = if let Some((cached_page, cached_rowid)) = self.append_hint_cache {
            if cached_page == hinted_leaf_page {
                cached_rowid
            } else {
                self.rowid(cx)?
            }
        } else {
            self.rowid(cx)?
        };
        if rowid <= last_rowid {
            self.stack.clear();
            self.at_eof = true;
            return Ok(false);
        }

        self.at_eof = true;
        let mut cell_data = std::mem::take(&mut self.cell_buf);
        let overflow_head = self.encode_table_leaf_cell_into(cx, rowid, data, &mut cell_data)?;

        match self.try_append_on_current_leaf(cx, &cell_data) {
            Ok(true) => {
                self.cell_buf = cell_data;
                self.last_insert_rowid = Some(rowid);
                // bd-wwqen.3: cache the leaf page + rowid for next insert.
                self.append_hint_cache = Some((hinted_leaf_page, rowid));
                Ok(true)
            }
            Ok(false) => {
                self.cell_buf = cell_data;
                if let Some(first) = overflow_head {
                    self.free_overflow_chain(cx, first)?;
                }
                self.stack.clear();
                self.at_eof = true;
                self.append_hint_cache = None; // leaf full or stale
                Ok(false)
            }
            Err(error) => {
                self.cell_buf = cell_data;
                if let Some(first) = overflow_head {
                    let _ = self.free_overflow_chain(cx, first);
                }
                self.stack.clear();
                self.at_eof = true;
                Err(error)
            }
        }
    }

    /// Fast insert path for callers that already positioned the cursor with
    /// `table_move_to(rowid)` and observed `SeekResult::NotFound`.
    ///
    /// This reuses the current successor/EOF position instead of performing a
    /// second full B-tree seek before the insert. The VDBE `Insert` path uses
    /// this as its `USESEEKRESULT`-style successor-position reuse primitive.
    #[doc(hidden)]
    pub fn table_insert_prechecked_absent(
        &mut self,
        cx: &Cx,
        rowid: i64,
        data: &[u8],
    ) -> Result<()> {
        self.with_btree_op(cx, BtreeOpType::Insert, |cursor| {
            cursor.table_insert_from_current_position(cx, rowid, data)
        })
    }

    /// Fast insert path for callers that already positioned the cursor with
    /// `index_move_to(key)` and observed `SeekResult::NotFound`.
    ///
    /// This reuses the current successor/EOF position instead of performing a
    /// second full B-tree seek before the insert.
    #[doc(hidden)]
    pub fn index_insert_prechecked_absent(&mut self, cx: &Cx, key: &[u8]) -> Result<()> {
        self.with_btree_op(cx, BtreeOpType::Insert, |cursor| {
            cursor.index_insert_from_current_position(cx, key)
        })
    }

    /// Fast insert path for callers that expect a monotonically increasing
    /// rowid stream and want to try the rightmost-leaf append path before
    /// falling back to a full seek.
    #[doc(hidden)]
    pub fn table_insert_rightmost_hint(&mut self, cx: &Cx, rowid: i64, data: &[u8]) -> Result<()> {
        self.with_btree_op(cx, BtreeOpType::Insert, |cursor| {
            let has_last = cursor.last(cx)?;
            if has_last {
                let last_rowid = cursor.rowid(cx)?;
                if rowid <= last_rowid {
                    let seek = cursor.table_seek_for_insert(cx, rowid)?;
                    if seek.is_found() {
                        return Err(FrankenError::PrimaryKeyViolation);
                    }
                    return cursor.table_insert_from_current_position(cx, rowid, data);
                }
                cursor.at_eof = true;
            }
            cursor.table_insert_from_current_position(cx, rowid, data)
        })
    }

    /// Fast append path for repeated monotonic inserts when the caller has a
    /// previously successful rightmost leaf hint from the same table.
    ///
    /// The hint is conservative: if the leaf is stale, full, or no longer
    /// ordered before `rowid`, this falls back to the normal rightmost-hint
    /// insert path.
    #[doc(hidden)]
    pub fn table_insert_rightmost_leaf_hint(
        &mut self,
        cx: &Cx,
        hinted_leaf_page: PageNumber,
        rowid: i64,
        data: &[u8],
    ) -> Result<()> {
        self.with_btree_op(cx, BtreeOpType::Insert, |cursor| {
            if cursor.try_table_append_on_hinted_leaf(cx, hinted_leaf_page, rowid, data)? {
                return Ok(());
            }

            let has_last = cursor.last(cx)?;
            if has_last {
                let last_rowid = cursor.rowid(cx)?;
                if rowid <= last_rowid {
                    let seek = cursor.table_seek_for_insert(cx, rowid)?;
                    if seek.is_found() {
                        return Err(FrankenError::PrimaryKeyViolation);
                    }
                    return cursor.table_insert_from_current_position(cx, rowid, data);
                }
                cursor.at_eof = true;
            }
            cursor.table_insert_from_current_position(cx, rowid, data)
        })
    }
}

// ---------------------------------------------------------------------------
// BtreeCursorOps implementation
// ---------------------------------------------------------------------------

impl<P: PageWriter> sealed::Sealed for BtCursor<P> {}

#[allow(clippy::missing_errors_doc)]
impl<P: PageWriter> BtreeCursorOps for BtCursor<P> {
    fn index_move_to(&mut self, cx: &Cx, key: &[u8]) -> Result<SeekResult> {
        self.with_btree_op(cx, BtreeOpType::Seek, |cursor| cursor.index_seek(cx, key))
    }

    fn table_move_to(&mut self, cx: &Cx, rowid: i64) -> Result<SeekResult> {
        self.with_btree_op(cx, BtreeOpType::Seek, |cursor| cursor.table_seek(cx, rowid))
    }

    fn first(&mut self, cx: &Cx) -> Result<bool> {
        observe_cursor_cancellation(cx)?;
        self.stack.clear();
        self.at_eof = true;
        self.move_to_leftmost_leaf(cx, self.root_page, true)
    }

    fn last(&mut self, cx: &Cx) -> Result<bool> {
        observe_cursor_cancellation(cx)?;
        self.stack.clear();
        self.at_eof = true;
        self.move_to_rightmost_leaf(cx, self.root_page, true)
    }

    fn next(&mut self, cx: &Cx) -> Result<bool> {
        self.advance_next(cx)
    }

    fn prev(&mut self, cx: &Cx) -> Result<bool> {
        self.advance_prev(cx)
    }

    fn index_insert(&mut self, cx: &Cx, key: &[u8]) -> Result<()> {
        self.with_btree_op(cx, BtreeOpType::Insert, |cursor| {
            let seek = cursor.index_seek_for_insert(cx, key)?;
            let (is_leaf, cell_idx) = {
                let top = cursor
                    .stack
                    .last()
                    .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
                (top.header.page_type.is_leaf(), top.cell_idx)
            };

            let mut insert_idx;

            if !is_leaf {
                if seek.is_found() {
                    // Matched exactly on an interior page. Descend to the right child's leftmost leaf.
                    let right_child = {
                        let top = cursor
                            .stack
                            .last()
                            .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
                        cursor.child_page_at(top, cell_idx + 1)?
                    };
                    cursor.move_to_leftmost_leaf(cx, right_child, false)?;

                    insert_idx = 0; // The new key goes at the very beginning of the right subtree.
                } else {
                    // Successor on an interior page. The key belongs in the LEFT child's rightmost leaf.
                    let left_child = {
                        let top = cursor
                            .stack
                            .last()
                            .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
                        cursor.child_page_at(top, cell_idx)?
                    };
                    cursor.move_to_rightmost_leaf(cx, left_child, false)?;
                    let top = cursor
                        .stack
                        .last()
                        .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
                    insert_idx = top.header.cell_count; // Append at the end of the left child.
                }
            } else {
                let top = cursor
                    .stack
                    .last()
                    .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?;
                insert_idx = if cursor.at_eof {
                    top.header.cell_count
                } else {
                    top.cell_idx
                };
                if seek.is_found() {
                    // Duplicate key on a leaf; place after the existing one.
                    insert_idx = insert_idx.saturating_add(1);
                }
            }

            // Take cell_buf for reuse — same pattern as table_insert.
            let mut cell_data = std::mem::take(&mut cursor.cell_buf);
            let overflow_head = cursor.encode_index_leaf_cell_into(cx, key, &mut cell_data)?;

            match cursor.try_insert_on_leaf(cx, insert_idx, &cell_data) {
                Ok(true) => {
                    cursor.cell_buf = cell_data;
                    Ok(())
                }
                Ok(false) => {
                    // Page full — balance and redistribute.
                    let balance_result = cursor.balance_for_insert(cx, &cell_data, insert_idx);
                    cursor.cell_buf = cell_data;
                    if balance_result.is_err() {
                        if let Some(first) = overflow_head {
                            let _ = cursor.free_overflow_chain(cx, first);
                        }
                    }
                    balance_result
                }
                Err(error) => {
                    cursor.cell_buf = cell_data;
                    if let Some(first) = overflow_head {
                        let _ = cursor.free_overflow_chain(cx, first);
                    }
                    Err(error)
                }
            }
        })
    }

    fn index_insert_unique(
        &mut self,
        cx: &Cx,
        key: &[u8],
        n_unique_cols: usize,
        columns_label: &str,
    ) -> Result<()> {
        let _record_profile_scope = enter_record_profile_scope(RecordProfileScope::BtreeCursor);
        // Parse the new key to extract the indexed column values.
        let new_fields = match parse_record(key) {
            Some(f) => f,
            None => return self.index_insert(cx, key),
        };
        // Check that all indexed columns are non-NULL — if any is NULL,
        // SQLite allows the insert regardless of uniqueness.
        let any_null = new_fields
            .iter()
            .take(n_unique_cols)
            .any(|v| matches!(v, fsqlite_types::SqliteValue::Null));
        if any_null {
            return self.index_insert(cx, key);
        }

        let new_prefix = &new_fields[..n_unique_cols.min(new_fields.len())];

        // Use index_seek to position cursor, then scan adjacent entries for
        // prefix matches. We check the current entry and the predecessor.
        // Because the full key includes the rowid suffix, two records with the
        // same indexed columns but different rowids sort adjacently.
        self.with_btree_op(cx, BtreeOpType::Seek, |cursor| {
            let _seek = cursor.index_seek(cx, key)?;
            let restore_eof = cursor.at_eof;

            let mut to_check = Vec::with_capacity(2);

            if !cursor.at_eof {
                to_check.push(cursor.payload(cx)?);
            }

            if cursor.prev(cx)? {
                to_check.push(cursor.payload(cx)?);
            }

            for existing_key in to_check {
                if let Some(existing_fields) = parse_record(&existing_key) {
                    if existing_fields.len() >= n_unique_cols
                        && new_prefix == &existing_fields[..n_unique_cols]
                    {
                        return Err(FrankenError::UniqueViolation {
                            columns: columns_label.to_owned(),
                        });
                    }
                }
            }

            if restore_eof {
                cursor.at_eof = true;
            } else {
                cursor.next(cx)?;
            }
            Ok(())
        })?;

        // bd-wwqen.3: the post-probe successor/EOF state from index_insert_unique()
        // is not reliable enough for deep monotonic unique-key streams. Reuse here
        // undercounts the index on the exact 10k unique-email workload, so route
        // unique inserts back through the canonical full insert path until a
        // stronger reuse contract is proven.
        self.index_insert(cx, key)
    }

    fn table_insert(&mut self, cx: &Cx, rowid: i64, data: &[u8]) -> Result<()> {
        self.with_btree_op(cx, BtreeOpType::Insert, |cursor| {
            // ── Normal path: full B-tree seek ────────────────────────
            let seek = cursor.table_seek_for_insert(cx, rowid)?;
            if seek.is_found() {
                return Err(FrankenError::PrimaryKeyViolation);
            }
            cursor.table_insert_from_current_position(cx, rowid, data)
        })
    }

    fn delete(&mut self, cx: &Cx) -> Result<()> {
        self.with_btree_op(cx, BtreeOpType::Delete, |cursor| {
            if cursor.at_eof || cursor.stack.is_empty() {
                return Err(FrankenError::internal("cursor at EOF"));
            }

            let top = cursor
                .stack
                .last()
                .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?
                .clone();

            if !top.header.page_type.is_leaf() {
                // Interior node deletion (index B-trees):
                // 1) identify successor payload, 2) replace interior key,
                // 3) remove duplicate successor from leaf.
                let original_key = cursor.payload(cx)?;

                // Advance to the successor in the right subtree.
                let advanced = cursor.advance_next(cx)?;
                if !advanced || cursor.at_eof {
                    return Err(FrankenError::DatabaseCorrupt {
                        detail: "no successor for interior node".to_owned(),
                    });
                }
                let successor_key = cursor.payload(cx)?;

                // Re-seek the original key to perform in-place interior replacement.
                let seek_res = cursor.index_seek(cx, &original_key)?;
                if !seek_res.is_found() {
                    return Err(FrankenError::DatabaseCorrupt {
                        detail: "original key disappeared during interior delete".to_owned(),
                    });
                }

                let top_after = cursor
                    .stack
                    .last()
                    .ok_or_else(|| FrankenError::internal("cursor stack is empty"))?
                    .clone();
                if top_after.header.page_type.is_leaf() {
                    return Err(FrankenError::DatabaseCorrupt {
                        detail: "interior delete re-seek landed on leaf".to_owned(),
                    });
                }

                // Replace the interior key first so failures do not lose both keys.
                let rebalanced = cursor.replace_interior_cell(cx, &successor_key)?;

                if rebalanced {
                    // The replacement triggered a rebalance on the interior node, which
                    // invalidated the cursor stack. We must re-seek to the successor key
                    // to find the duplicate leaf entry.
                    let seek_res = cursor.index_seek(cx, &successor_key)?;
                    if !seek_res.is_found() {
                        return Err(FrankenError::DatabaseCorrupt {
                            detail: "duplicate successor missing after interior rebalance"
                                .to_owned(),
                        });
                    }

                    let top_after_seek = cursor
                        .stack
                        .last()
                        .ok_or_else(|| {
                            FrankenError::internal("cursor stack empty after reseek in delete")
                        })?
                        .clone();
                    if !top_after_seek.header.page_type.is_leaf() {
                        // The seek found the interior node separator we just inserted.
                        // We must advance to the next logical entry to reach the duplicate in the leaf.
                        let successor_found = cursor.advance_next(cx)?;
                        if !successor_found || cursor.at_eof {
                            return Err(FrankenError::DatabaseCorrupt {
                                detail: "duplicate leaf successor missing after interior rebalance"
                                    .to_owned(),
                            });
                        }
                    }
                } else {
                    // The duplicate successor still exists as the next logical entry
                    // in the right subtree. Walk there from the interior replacement
                    // site instead of re-seeking, which would land back on the new
                    // interior separator.
                    let successor_found = cursor.advance_next(cx)?;
                    if !successor_found || cursor.at_eof {
                        return Err(FrankenError::DatabaseCorrupt {
                            detail: "duplicate successor missing after interior replacement"
                                .to_owned(),
                        });
                    }
                    let duplicate_successor = cursor.payload(cx)?;
                    if duplicate_successor != successor_key {
                        return Err(FrankenError::DatabaseCorrupt {
                            detail: "interior delete advanced to wrong successor duplicate"
                                .to_owned(),
                        });
                    }
                }

                // Remove the duplicate leaf successor.
                let (_leaf_pgno, new_count) = cursor.remove_cell_from_leaf(cx)?;
                if new_count == 0 {
                    cursor.balance_for_delete(cx)?;
                }

                // Delete contract: position cursor at the next logical entry.
                let _ = cursor.index_seek(cx, &successor_key)?;
                return Ok(());
            }

            // If the leaf is about to become empty (and it's not the root),
            // balancing will occur, which clears the cursor stack (invalidating position).
            // We must save the current key/rowid to re-establish position (at the successor)
            // after balancing.
            let depth = cursor.stack.len();
            let needs_anchor = depth > 1 && top.header.cell_count == 1;

            #[allow(clippy::items_after_statements)]
            enum Anchor {
                Rowid(i64),
                Key(Vec<u8>),
            }
            let anchor = if needs_anchor {
                if cursor.is_table {
                    Some(Anchor::Rowid(cursor.rowid(cx)?))
                } else {
                    Some(Anchor::Key(cursor.payload(cx)?))
                }
            } else {
                None
            };

            // Remove the cell from the leaf. This handles overflow chain
            // cleanup and refreshes the stack entry.
            let (_leaf_page_no, new_count) = cursor.remove_cell_from_leaf(cx)?;

            // Trigger structural rebalance only when a non-root leaf drains.
            // This avoids aggressive full-sibling rewrites on every delete while
            // still fixing the "empty leftmost leaf breaks first()" failure mode.
            if new_count == 0 {
                cursor.balance_for_delete(cx)?;

                // If we balanced, the stack was cleared. Re-seek to the anchor.
                // Since the anchor key was just deleted, the seek will land on
                // the *next* entry (or EOF), which is exactly what we want.
                if let Some(anc) = anchor {
                    match anc {
                        Anchor::Rowid(r) => {
                            cursor.table_move_to(cx, r)?;
                        }
                        Anchor::Key(k) => {
                            cursor.index_move_to(cx, &k)?;
                        }
                    }
                }
            }

            Ok(())
        })
    }

    fn payload(&self, cx: &Cx) -> Result<Vec<u8>> {
        if self.at_eof || self.stack.is_empty() {
            return Err(FrankenError::internal("cursor at EOF"));
        }
        let top = self
            .stack
            .last()
            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?;
        let cell = self.parse_cell_at(top, top.cell_idx)?;
        match self.read_cell_payload(cx, top, &cell)? {
            Cow::Borrowed(bytes) => {
                instrumentation::record_owned_payload_materialization(bytes.len());
                Ok(bytes.to_vec())
            }
            Cow::Owned(bytes) => Ok(bytes),
        }
    }

    fn payload_into(&self, cx: &Cx, buf: &mut Vec<u8>) -> Result<()> {
        if self.at_eof || self.stack.is_empty() {
            return Err(FrankenError::internal("cursor at EOF"));
        }
        let top = self
            .stack
            .last()
            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?;
        let cell = self.parse_cell_at(top, top.cell_idx)?;

        self.read_cell_payload_into(cx, top, &cell, buf)
    }

    fn payload_prefix_into(
        &self,
        cx: &Cx,
        max_prefix_bytes: usize,
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        if self.at_eof || self.stack.is_empty() {
            return Err(FrankenError::internal("cursor at EOF"));
        }
        let top = self
            .stack
            .last()
            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?;
        let cell = self.parse_cell_at(top, top.cell_idx)?;

        self.read_cell_payload_prefix_into(cx, top, &cell, max_prefix_bytes, buf)
    }

    fn rowid(&self, cx: &Cx) -> Result<i64> {
        let _record_profile_scope = enter_record_profile_scope(RecordProfileScope::BtreeCursor);
        if self.at_eof || self.stack.is_empty() {
            return Err(FrankenError::internal("cursor at EOF"));
        }
        let top = self
            .stack
            .last()
            .ok_or_else(|| FrankenError::internal("cursor stack empty"))?;
        let cell = self.parse_cell_at(top, top.cell_idx)?;
        if let Some(rowid) = cell.rowid {
            return Ok(rowid);
        }

        // Index cursor: rowid is stored as the trailing field in the
        // serialized key record.
        let key = self.read_cell_payload(cx, top, &cell)?;
        let key_values =
            parse_record(key.as_ref()).ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: "malformed index key record while extracting rowid".to_owned(),
            })?;
        key_values
            .last()
            .and_then(fsqlite_types::SqliteValue::as_integer)
            .ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: "index key record missing trailing integer rowid".to_owned(),
            })
    }

    fn eof(&self) -> bool {
        self.at_eof
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::instrumentation::btree_metrics_snapshot;
    use fsqlite_pager::{MemoryMockMvccPager, MockMvccPager, MvccPager as _, TransactionMode};
    use fsqlite_types::SqliteValue;
    use fsqlite_types::record::serialize_record;
    use fsqlite_types::serial_type::write_varint;
    use proptest::strategy::Strategy as _;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    // MemPageStore is now defined at module scope (pub) and imported via
    // `use super::*;`.  Tests use `MemPageStore::new(USABLE)` instead of
    // the former `MemPageStore::new(USABLE)`.

    #[derive(Debug, Clone)]
    struct PrefetchProbeStore {
        inner: MemPageStore,
        hinted_pages: Rc<RefCell<Vec<PageNumber>>>,
    }

    impl PrefetchProbeStore {
        fn new(inner: MemPageStore) -> Self {
            Self {
                inner,
                hinted_pages: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn hinted_pages(&self) -> Vec<PageNumber> {
            self.hinted_pages.borrow().clone()
        }
    }

    impl PageReader for PrefetchProbeStore {
        fn read_page(&self, cx: &Cx, page_no: PageNumber) -> Result<Vec<u8>> {
            self.inner.read_page(cx, page_no)
        }

        fn prefetch_page_hint(&self, _cx: &Cx, page_no: PageNumber) {
            self.hinted_pages.borrow_mut().push(page_no);
        }
    }

    impl PageWriter for PrefetchProbeStore {
        fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
            self.inner.write_page(cx, page_no, data)
        }

        fn allocate_page(&mut self, cx: &Cx) -> Result<PageNumber> {
            self.inner.allocate_page(cx)
        }

        fn free_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<()> {
            self.inner.free_page(cx, page_no)
        }

        fn record_write_witness(&mut self, _cx: &Cx, _key: WitnessKey) {}
    }

    #[derive(Debug)]
    struct FailingOverflowStore {
        inner: Rc<RefCell<MemPageStore>>,
        fail_on_write: usize,
        write_count: usize,
    }

    impl FailingOverflowStore {
        fn new(inner: Rc<RefCell<MemPageStore>>, fail_on_write: usize) -> Self {
            Self {
                inner,
                fail_on_write,
                write_count: 0,
            }
        }
    }

    impl PageReader for FailingOverflowStore {
        fn read_page(&self, cx: &Cx, page_no: PageNumber) -> Result<Vec<u8>> {
            self.inner.borrow().read_page(cx, page_no)
        }
    }

    impl PageWriter for FailingOverflowStore {
        fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
            self.write_count = self.write_count.saturating_add(1);
            if self.write_count == self.fail_on_write {
                return Err(FrankenError::internal("injected write failure"));
            }
            self.inner.borrow_mut().write_page(cx, page_no, data)
        }

        fn allocate_page(&mut self, cx: &Cx) -> Result<PageNumber> {
            self.inner.borrow_mut().allocate_page(cx)
        }

        fn free_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<()> {
            self.inner.borrow_mut().free_page(cx, page_no)
        }

        fn record_write_witness(&mut self, _cx: &Cx, _key: WitnessKey) {}
    }

    #[derive(Debug, Clone)]
    struct CancelAfterReadStore {
        inner: MemPageStore,
        cancelled: Rc<RefCell<bool>>,
    }

    impl CancelAfterReadStore {
        fn new(inner: MemPageStore) -> Self {
            Self {
                inner,
                cancelled: Rc::new(RefCell::new(false)),
            }
        }
    }

    impl PageReader for CancelAfterReadStore {
        fn read_page(&self, cx: &Cx, page_no: PageNumber) -> Result<Vec<u8>> {
            let page = self.inner.read_page(cx, page_no)?;
            let mut cancelled = self.cancelled.borrow_mut();
            if !*cancelled {
                cx.cancel();
                *cancelled = true;
            }
            Ok(page)
        }

        fn prefetch_page_hint(&self, cx: &Cx, page_no: PageNumber) {
            self.inner.prefetch_page_hint(cx, page_no);
        }
    }

    impl PageWriter for CancelAfterReadStore {
        fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
            self.inner.write_page(cx, page_no, data)
        }

        fn allocate_page(&mut self, cx: &Cx) -> Result<PageNumber> {
            self.inner.allocate_page(cx)
        }

        fn free_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<()> {
            self.inner.free_page(cx, page_no)
        }

        fn record_write_witness(&mut self, cx: &Cx, key: WitnessKey) {
            self.inner.record_write_witness(cx, key);
        }
    }

    #[derive(Debug)]
    struct CancelAfterFirstOverflowFreeStore {
        inner: Rc<RefCell<MemPageStore>>,
        cancelled: Rc<RefCell<bool>>,
    }

    impl CancelAfterFirstOverflowFreeStore {
        fn new(inner: Rc<RefCell<MemPageStore>>) -> Self {
            Self {
                inner,
                cancelled: Rc::new(RefCell::new(false)),
            }
        }
    }

    impl PageReader for CancelAfterFirstOverflowFreeStore {
        fn read_page(&self, cx: &Cx, page_no: PageNumber) -> Result<Vec<u8>> {
            cx.checkpoint().map_err(|_| FrankenError::Abort)?;
            self.inner.borrow().read_page(cx, page_no)
        }
    }

    impl PageWriter for CancelAfterFirstOverflowFreeStore {
        fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
            self.inner.borrow_mut().write_page(cx, page_no, data)
        }

        fn allocate_page(&mut self, cx: &Cx) -> Result<PageNumber> {
            self.inner.borrow_mut().allocate_page(cx)
        }

        fn free_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<()> {
            cx.checkpoint().map_err(|_| FrankenError::Abort)?;
            self.inner.borrow_mut().free_page(cx, page_no)?;

            let mut cancelled = self.cancelled.borrow_mut();
            if !*cancelled {
                cx.cancel();
                *cancelled = true;
            }
            Ok(())
        }

        fn record_write_witness(&mut self, cx: &Cx, key: WitnessKey) {
            self.inner.borrow_mut().record_write_witness(cx, key);
        }
    }

    const USABLE: u32 = 4096;

    fn collect_reachable_pages(
        store: &MemPageStore,
        page_no: PageNumber,
        usable_size: u32,
        out: &mut BTreeSet<u32>,
    ) {
        if !out.insert(page_no.get()) {
            return;
        }

        let page = store
            .pages
            .get(&page_no.get())
            .expect("reachable page should exist in store");
        let header_offset = cell::header_offset_for_page(page_no);
        let header = BtreePageHeader::parse(page, header_offset).expect("page header should parse");

        if !header.page_type.is_interior() {
            return;
        }

        let ptrs =
            cell::read_cell_pointers(page, &header, header_offset).expect("cell pointers parse");
        for ptr in ptrs {
            let cell = CellRef::parse(page, usize::from(ptr), header.page_type, usable_size)
                .expect("interior cell should parse");
            let child = cell
                .left_child
                .expect("interior cell should reference child");
            collect_reachable_pages(store, child, usable_size, out);
        }
        let right_child = header
            .right_child
            .expect("interior page should reference right child");
        collect_reachable_pages(store, right_child, usable_size, out);
    }

    #[test]
    fn test_btree_observability_operation_totals() {
        let before = btree_metrics_snapshot();

        let cx = Cx::new();
        let root = PageNumber::new(2).unwrap();
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);

        cursor.table_insert(&cx, 7, b"payload").unwrap();
        assert!(cursor.table_move_to(&cx, 7).unwrap().is_found());
        cursor.delete(&cx).unwrap();

        let after = btree_metrics_snapshot();
        assert!(
            after.fsqlite_btree_operations_total.seek
                >= before.fsqlite_btree_operations_total.seek.saturating_add(1)
        );
        assert!(
            after.fsqlite_btree_operations_total.insert
                >= before
                    .fsqlite_btree_operations_total
                    .insert
                    .saturating_add(1)
        );
        assert!(
            after.fsqlite_btree_operations_total.delete
                >= before
                    .fsqlite_btree_operations_total
                    .delete
                    .saturating_add(1)
        );
        assert!(after.fsqlite_btree_depth >= 1);
    }

    #[test]
    fn test_btree_observability_split_counter_and_depth_gauge() {
        let before = btree_metrics_snapshot();

        let cx = Cx::new();
        let root = PageNumber::new(2).unwrap();
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);

        // Use large payloads to force page splits quickly.  With 9KB
        // payloads on a 4096-byte page, each insert uses overflow pages
        // and the root splits after just a few rows.
        let payload = vec![0xAB; 9_000];
        let mut inserts_done: i64 = 0;
        for rowid in 1_i64..=500_i64 {
            cursor.table_insert(&cx, rowid, &payload).unwrap();
            inserts_done = rowid;

            let root_page = cursor.pager.pages.get(&2).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorTable {
                break;
            }

            assert!(
                rowid < 1000,
                "table root did not split under sustained inserts"
            );
        }

        let snapshot = btree_metrics_snapshot();
        assert!(
            snapshot.fsqlite_btree_page_splits_total > before.fsqlite_btree_page_splits_total,
            "expected at least one split when loading large rows"
        );
        // The insert counter must have increased by at least the number
        // of rows we actually inserted (the loop may break early when
        // the root splits).
        assert!(
            snapshot.fsqlite_btree_operations_total.insert
                >= before
                    .fsqlite_btree_operations_total
                    .insert
                    .saturating_add(inserts_done as u64),
            "insert counter should reflect at least {inserts_done} inserts"
        );
    }

    #[test]
    fn test_table_insert_prechecked_absent_reuses_successor_position() {
        let cx = Cx::new();
        let root = PageNumber::new(2).unwrap();
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);

        cursor.table_insert(&cx, 10, b"ten").unwrap();
        cursor.table_insert(&cx, 30, b"thirty").unwrap();

        let seek = cursor.table_move_to(&cx, 20).unwrap();
        assert_eq!(seek, SeekResult::NotFound);
        assert!(!cursor.eof(), "seek should land on successor rowid 30");

        cursor
            .table_insert_prechecked_absent(&cx, 20, b"twenty")
            .unwrap();

        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 10);
        assert_eq!(cursor.payload(&cx).unwrap(), b"ten");

        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 20);
        assert_eq!(cursor.payload(&cx).unwrap(), b"twenty");

        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 30);
        assert_eq!(cursor.payload(&cx).unwrap(), b"thirty");
    }

    #[test]
    fn test_table_insert_prechecked_absent_reuses_eof_position() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (2, b"two"), (3, b"three"), (4, b"four")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let seek = cursor.table_move_to(&cx, 99).unwrap();
        assert_eq!(seek, SeekResult::NotFound);
        assert!(
            cursor.eof(),
            "seek past end should preserve EOF insertion context"
        );

        cursor
            .table_insert_prechecked_absent(&cx, 99, b"tail")
            .unwrap();

        assert!(cursor.table_move_to(&cx, 99).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"tail");
    }

    #[test]
    fn test_table_insert_prechecked_absent_deep_tree_rightmost_10k() {
        let cx = Cx::new();
        let root = PageNumber::new(2).unwrap();
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);
        let row_count = 10_000_i64;

        for rowid in 0..row_count {
            let payload = format!("row-{rowid}");
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert_eq!(
                seek,
                SeekResult::NotFound,
                "monotonic prechecked insert should not find existing rowid {rowid}"
            );
            cursor
                .table_insert_prechecked_absent(&cx, rowid, payload.as_bytes())
                .unwrap();
        }

        let counted = cursor.count_all_rows(&cx).unwrap();
        assert_eq!(
            counted, row_count,
            "deep/rightmost prechecked-absent table with {row_count} rows must count exactly"
        );

        let seek = cursor.table_move_to(&cx, row_count - 1).unwrap();
        assert_eq!(seek, SeekResult::Found);
        assert_eq!(
            cursor.payload(&cx).unwrap(),
            format!("row-{}", row_count - 1).as_bytes()
        );
    }

    /// Helper: build a leaf table page with sorted (rowid, payload) entries.
    fn build_leaf_table(entries: &[(i64, &[u8])]) -> Vec<u8> {
        let mut page = vec![0u8; USABLE as usize];
        let header_size = 8usize; // leaf

        // Build cells from the end of the page.
        let mut cell_end = USABLE as usize;
        let mut cell_offsets: Vec<u16> = Vec::new();

        for &(rowid, payload) in entries {
            // Cell: [payload_size varint] [rowid varint] [payload]
            let mut cell = Vec::new();
            let mut vbuf = [0u8; 9];
            let n = write_varint(&mut vbuf, payload.len() as u64);
            cell.extend_from_slice(&vbuf[..n]);
            #[allow(clippy::cast_sign_loss)]
            let n = write_varint(&mut vbuf, rowid as u64);
            cell.extend_from_slice(&vbuf[..n]);
            cell.extend_from_slice(payload);

            cell_end -= cell.len();
            page[cell_end..cell_end + cell.len()].copy_from_slice(&cell);
            cell_offsets.push(cell_end as u16);
        }

        // Write header.
        page[0] = 0x0D; // LeafTable
        page[1..3].copy_from_slice(&0u16.to_be_bytes()); // no freeblock
        #[allow(clippy::cast_possible_truncation)]
        let cell_count = entries.len() as u16;
        page[3..5].copy_from_slice(&cell_count.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        let content_offset = cell_end as u16;
        page[5..7].copy_from_slice(&content_offset.to_be_bytes());
        page[7] = 0; // fragmented bytes

        // Write cell pointer array.
        for (i, &off) in cell_offsets.iter().enumerate() {
            let ptr_offset = header_size + i * 2;
            page[ptr_offset..ptr_offset + 2].copy_from_slice(&off.to_be_bytes());
        }

        page
    }

    #[test]
    fn test_transaction_page_io_reads_bytes_from_transaction_handle() {
        let cx = Cx::new();
        let pager = MockMvccPager;
        let mut txn = pager
            .begin(&cx, TransactionMode::Deferred)
            .expect("mock transaction begin should succeed");
        let page_no = PageNumber::new(42).expect("page number must be non-zero");

        let io = TransactionPageIo::new(&mut txn);
        let bytes = io
            .read_page(&cx, page_no)
            .expect("read_page should forward to transaction handle");

        assert_eq!(
            bytes.get(..4),
            Some(&page_no.get().to_le_bytes()[..]),
            "TransactionHandle::get_page stamps page number in first 4 bytes"
        );
    }

    #[test]
    fn test_transaction_page_io_writes_owned_page_data_via_transaction_handle() {
        let cx = Cx::new();
        let pager = MemoryMockMvccPager;
        let mut txn = pager
            .begin(&cx, TransactionMode::Deferred)
            .expect("mock transaction begin should succeed");
        let page_no = PageNumber::new(2).expect("page number must be non-zero");
        let expected = vec![0xAB; 32];

        let mut io = TransactionPageIo::new(&mut txn);
        io.write_page_data(&cx, page_no, PageData::from_vec(expected.clone()))
            .expect("write_page_data should forward");

        let bytes = io
            .read_page(&cx, page_no)
            .expect("read_page should return the owned bytes");
        assert_eq!(
            bytes.len(),
            fsqlite_types::PageSize::default().as_usize(),
            "owned-page writes should preserve the page-size invariant"
        );
        assert_eq!(&bytes[..expected.len()], expected.as_slice());
        assert!(
            bytes[expected.len()..].iter().all(|byte| *byte == 0),
            "owned-page writes should zero-fill any unwritten tail bytes"
        );
    }

    #[test]
    fn test_mem_page_store_write_page_short_buffer_is_zero_filled_to_page_size() {
        let cx = Cx::new();
        let page_size = 128_u32;
        let mut store = MemPageStore::new(page_size);
        let page_no = PageNumber::new(2).expect("page number must be non-zero");
        let expected = vec![0xCD; 32];

        store
            .write_page(&cx, page_no, &expected)
            .expect("write_page should normalize short buffers");

        let bytes = store
            .read_page(&cx, page_no)
            .expect("read_page should return normalized page bytes");
        assert_eq!(
            bytes.len(),
            page_size as usize,
            "raw write_page should preserve the page-size invariant"
        );
        assert_eq!(&bytes[..expected.len()], expected.as_slice());
        assert!(
            bytes[expected.len()..].iter().all(|byte| *byte == 0),
            "raw write_page should zero-fill any unwritten tail bytes"
        );
    }

    #[test]
    fn test_transaction_page_io_allocates_pages_via_transaction_handle() {
        let cx = Cx::new();
        let pager = MockMvccPager;
        let mut txn = pager
            .begin(&cx, TransactionMode::Deferred)
            .expect("mock transaction begin should succeed");

        let mut io = TransactionPageIo::new(&mut txn);
        let first = io.allocate_page(&cx).expect("allocate_page should forward");
        let second = io.allocate_page(&cx).expect("allocate_page should forward");

        assert_eq!(first.get(), 2, "mock allocator starts at page 2");
        assert_eq!(second.get(), 3, "mock allocator increments page numbers");
    }

    #[test]
    fn test_transaction_page_io_writes_and_frees_via_transaction_handle() {
        let cx = Cx::new();
        let pager = MockMvccPager;
        let mut txn = pager
            .begin(&cx, TransactionMode::Deferred)
            .expect("mock transaction begin should succeed");
        let page_no = PageNumber::new(2).expect("page number must be non-zero");

        let mut io = TransactionPageIo::new(&mut txn);
        io.write_page(&cx, page_no, &[0_u8; 32])
            .expect("write_page should forward");
        io.free_page(&cx, page_no)
            .expect("free_page should forward");
    }

    /// Helper: build an interior table page.
    ///
    /// `children` is a list of `(left_child, rowid)` pairs plus a final right_child.
    fn build_interior_table(children: &[(PageNumber, i64)], right_child: PageNumber) -> Vec<u8> {
        let mut page = vec![0u8; USABLE as usize];
        let header_size = 12usize; // interior

        let mut cell_end = USABLE as usize;
        let mut cell_offsets: Vec<u16> = Vec::new();

        for &(left_child, rowid) in children {
            // Interior table cell: [left_child: u32 BE] [rowid: varint]
            let mut cell = Vec::new();
            cell.extend_from_slice(&left_child.get().to_be_bytes());
            let mut vbuf = [0u8; 9];
            #[allow(clippy::cast_sign_loss)]
            let n = write_varint(&mut vbuf, rowid as u64);
            cell.extend_from_slice(&vbuf[..n]);

            cell_end -= cell.len();
            page[cell_end..cell_end + cell.len()].copy_from_slice(&cell);
            cell_offsets.push(cell_end as u16);
        }

        // Write header.
        page[0] = 0x05; // InteriorTable
        page[1..3].copy_from_slice(&0u16.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        let cell_count = children.len() as u16;
        page[3..5].copy_from_slice(&cell_count.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        let content_offset = cell_end as u16;
        page[5..7].copy_from_slice(&content_offset.to_be_bytes());
        page[7] = 0;
        page[8..12].copy_from_slice(&right_child.get().to_be_bytes());

        // Write cell pointer array.
        for (i, &off) in cell_offsets.iter().enumerate() {
            let ptr_offset = header_size + i * 2;
            page[ptr_offset..ptr_offset + 2].copy_from_slice(&off.to_be_bytes());
        }

        page
    }

    /// Helper: build an interior index page.
    ///
    /// `children` is a list of `(left_child, key)` pairs plus a final right_child.
    fn build_interior_index(children: &[(PageNumber, &[u8])], right_child: PageNumber) -> Vec<u8> {
        let mut page = vec![0u8; USABLE as usize];
        let header_size = 12usize; // interior

        let mut cell_end = USABLE as usize;
        let mut cell_offsets: Vec<u16> = Vec::new();

        for &(left_child, key) in children {
            // Interior index cell: [left_child: u32 BE] [payload_size varint] [payload]
            let mut cell = Vec::new();
            cell.extend_from_slice(&left_child.get().to_be_bytes());
            let mut vbuf = [0u8; 9];
            let n = write_varint(&mut vbuf, key.len() as u64);
            cell.extend_from_slice(&vbuf[..n]);
            cell.extend_from_slice(key);

            cell_end -= cell.len();
            page[cell_end..cell_end + cell.len()].copy_from_slice(&cell);
            cell_offsets.push(cell_end as u16);
        }

        page[0] = 0x02; // InteriorIndex
        page[1..3].copy_from_slice(&0u16.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        let cell_count = children.len() as u16;
        page[3..5].copy_from_slice(&cell_count.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        let content_offset = cell_end as u16;
        page[5..7].copy_from_slice(&content_offset.to_be_bytes());
        page[7] = 0;
        page[8..12].copy_from_slice(&right_child.get().to_be_bytes());

        for (i, &off) in cell_offsets.iter().enumerate() {
            let ptr_offset = header_size + i * 2;
            page[ptr_offset..ptr_offset + 2].copy_from_slice(&off.to_be_bytes());
        }

        page
    }

    /// Helper: build a leaf index page with sorted key payloads.
    fn build_leaf_index(entries: &[&[u8]]) -> Vec<u8> {
        let mut page = vec![0u8; USABLE as usize];
        let header_size = 8usize; // leaf

        let mut cell_end = USABLE as usize;
        let mut cell_offsets: Vec<u16> = Vec::new();

        for &key in entries {
            let mut cell = Vec::new();
            let mut vbuf = [0u8; 9];
            let n = write_varint(&mut vbuf, key.len() as u64);
            cell.extend_from_slice(&vbuf[..n]);
            cell.extend_from_slice(key);

            cell_end -= cell.len();
            page[cell_end..cell_end + cell.len()].copy_from_slice(&cell);
            cell_offsets.push(cell_end as u16);
        }

        page[0] = 0x0A; // LeafIndex
        page[1..3].copy_from_slice(&0u16.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        let cell_count = entries.len() as u16;
        page[3..5].copy_from_slice(&cell_count.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        let content_offset = cell_end as u16;
        page[5..7].copy_from_slice(&content_offset.to_be_bytes());
        page[7] = 0;

        for (i, &off) in cell_offsets.iter().enumerate() {
            let ptr_offset = header_size + i * 2;
            page[ptr_offset..ptr_offset + 2].copy_from_slice(&off.to_be_bytes());
        }

        page
    }

    fn pn(n: u32) -> PageNumber {
        PageNumber::new(n).unwrap()
    }

    fn lcg_next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        *state
    }

    fn deterministic_shuffle(values: &mut [i64], seed: u64) {
        if values.len() <= 1 {
            return;
        }
        let mut state = seed;
        for i in (1..values.len()).rev() {
            let j = (lcg_next(&mut state) as usize) % (i + 1);
            values.swap(i, j);
        }
    }

    fn payload_for_rowid(rowid: i64) -> Vec<u8> {
        let rowid_usize = usize::try_from(rowid).expect("rowid must be positive in this test");
        let payload_len = if rowid % 257 == 0 {
            1_600 // force overflow-chain path for some keys
        } else {
            32 + (rowid_usize % 180)
        };

        let mut payload = Vec::with_capacity(payload_len);
        for i in 0..payload_len {
            let byte = (rowid_usize.wrapping_mul(31).wrapping_add(i * 17) & 0xFF) as u8;
            payload.push(byte);
        }
        payload
    }

    #[test]
    fn test_cursor_first_last_single_leaf() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"alice"), (5, b"bob"), (10, b"charlie")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert_eq!(cursor.payload(&cx).unwrap(), b"alice");

        assert!(cursor.last(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 10);
        assert_eq!(cursor.payload(&cx).unwrap(), b"charlie");
    }

    #[test]
    fn test_cursor_first_observes_cancelled_context_before_descent() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_leaf_table(&[(1, b"alice"), (5, b"bob")]));

        let cx = Cx::new();
        cx.cancel();

        let mut cursor = BtCursor::new(store, pn(2), USABLE, true);
        let err = cursor
            .first(&cx)
            .expect_err("cancelled context should abort before leaf descent");

        assert!(matches!(err, FrankenError::Abort));
    }

    #[test]
    fn test_table_seek_observes_cancellation_during_leaf_binary_search() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"alpha"), (5, b"bravo"), (9, b"charlie")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(CancelAfterReadStore::new(store), pn(2), USABLE, true);
        let err = cursor
            .table_move_to(&cx, 5)
            .expect_err("binary search should observe cancellation after page load");

        assert!(matches!(err, FrankenError::Abort));
    }

    #[test]
    fn test_index_seek_observes_cancellation_during_leaf_binary_search() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_leaf_index(&[b"alpha", b"bravo", b"charlie"]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(CancelAfterReadStore::new(store), pn(2), USABLE, false);
        let err = cursor
            .index_move_to(&cx, b"bravo")
            .expect_err("index binary search should observe cancellation after page load");

        assert!(matches!(err, FrankenError::Abort));
    }

    #[test]
    fn test_cursor_seek_exact() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (5, b"five"), (10, b"ten"), (15, b"fifteen")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        assert!(cursor.table_move_to(&cx, 5).unwrap().is_found());
        assert_eq!(cursor.rowid(&cx).unwrap(), 5);
        assert_eq!(cursor.payload(&cx).unwrap(), b"five");
    }

    #[test]
    fn test_cursor_seek_not_found() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (5, b"five"), (10, b"ten")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Seek for 3 — should land on 5 (next in sort order).
        let result = cursor.table_move_to(&cx, 3).unwrap();
        assert!(!result.is_found());
        assert!(!cursor.eof());
        assert_eq!(cursor.rowid(&cx).unwrap(), 5);

        // Seek for 20 — past the end.
        let result = cursor.table_move_to(&cx, 20).unwrap();
        assert!(!result.is_found());
        assert!(cursor.eof());
    }

    #[test]
    fn test_cursor_seek_observes_cancellation_during_leaf_binary_search() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (5, b"five"), (10, b"ten"), (15, b"fifteen")]),
        );

        let cancelled_cx = Cx::new();
        let mut cursor = BtCursor::new(CancelAfterReadStore::new(store), pn(2), USABLE, true);
        let err = cursor
            .table_move_to(&cancelled_cx, 10)
            .expect_err("cancellation should interrupt the in-node search loop");
        assert!(matches!(err, FrankenError::Abort));

        let recovery_cx = Cx::new();
        assert!(cursor.table_move_to(&recovery_cx, 10).unwrap().is_found());
        assert_eq!(cursor.rowid(&recovery_cx).unwrap(), 10);
    }

    #[test]
    fn test_cursor_first_observes_cancellation_during_descent_and_recovers() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 10)], pn(4)));
        store
            .pages
            .insert(3, build_leaf_table(&[(1, b"one"), (5, b"five")]));
        store
            .pages
            .insert(4, build_leaf_table(&[(10, b"ten"), (15, b"fifteen")]));

        let cancelled_cx = Cx::new();
        let mut cursor = BtCursor::new(CancelAfterReadStore::new(store), pn(2), USABLE, true);
        let err = cursor
            .first(&cancelled_cx)
            .expect_err("cancellation should interrupt multi-page descent");
        assert!(matches!(err, FrankenError::Abort));

        let recovery_cx = Cx::new();
        assert!(cursor.first(&recovery_cx).unwrap());
        assert_eq!(cursor.rowid(&recovery_cx).unwrap(), 1);
    }

    #[test]
    fn test_cursor_table_insert_single_leaf() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (3, b"three"), (5, b"five"), (7, b"seven")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        cursor.table_insert(&cx, 2, b"two").unwrap();

        assert!(cursor.table_move_to(&cx, 2).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"two");

        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 2);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 3);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 5);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 7);
        assert!(!cursor.next(&cx).unwrap());
    }

    #[test]
    fn test_cursor_table_insert_duplicate_rowid() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[(7, b"seven")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        let err = cursor.table_insert(&cx, 7, b"dupe").unwrap_err();
        assert!(matches!(err, FrankenError::PrimaryKeyViolation));
    }

    #[test]
    fn test_cursor_index_insert_single_leaf() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_leaf_index(&[b"apple", b"carrot", b"pear"]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, false);
        cursor.index_insert(&cx, b"banana").unwrap();

        assert!(cursor.index_move_to(&cx, b"banana").unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"banana");
    }

    #[test]
    fn test_index_insert_prechecked_absent_reuses_successor_position() {
        let store = MemPageStore::with_empty_index(pn(2), USABLE);
        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);

        let low_key = serialize_record(&[SqliteValue::Integer(10), SqliteValue::Integer(1)]);
        let mid_key = serialize_record(&[SqliteValue::Integer(20), SqliteValue::Integer(2)]);
        let high_key = serialize_record(&[SqliteValue::Integer(30), SqliteValue::Integer(3)]);

        cursor.index_insert(&cx, &low_key).unwrap();
        cursor.index_insert(&cx, &high_key).unwrap();

        let seek = cursor.index_move_to(&cx, &mid_key).unwrap();
        assert_eq!(seek, SeekResult::NotFound);
        assert!(!cursor.eof(), "seek should land on successor key");

        cursor
            .index_insert_prechecked_absent(&cx, &mid_key)
            .unwrap();

        assert!(cursor.first(&cx).unwrap());
        assert_eq!(
            parse_record(&cursor.payload(&cx).unwrap()).unwrap(),
            vec![SqliteValue::Integer(10), SqliteValue::Integer(1)]
        );
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(
            parse_record(&cursor.payload(&cx).unwrap()).unwrap(),
            vec![SqliteValue::Integer(20), SqliteValue::Integer(2)]
        );
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(
            parse_record(&cursor.payload(&cx).unwrap()).unwrap(),
            vec![SqliteValue::Integer(30), SqliteValue::Integer(3)]
        );
    }

    #[test]
    fn test_index_insert_unique_no_conflict_inserts_between_adjacent_prefixes() {
        let store = MemPageStore::with_empty_index(pn(2), USABLE);
        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);

        let low_key = serialize_record(&[SqliteValue::Integer(10), SqliteValue::Integer(1)]);
        let mid_key = serialize_record(&[SqliteValue::Integer(20), SqliteValue::Integer(2)]);
        let high_key = serialize_record(&[SqliteValue::Integer(30), SqliteValue::Integer(3)]);

        cursor.index_insert(&cx, &low_key).unwrap();
        cursor.index_insert(&cx, &high_key).unwrap();
        cursor.index_insert_unique(&cx, &mid_key, 1, "t.x").unwrap();

        assert!(cursor.first(&cx).unwrap());
        assert_eq!(
            parse_record(&cursor.payload(&cx).unwrap()).unwrap(),
            vec![SqliteValue::Integer(10), SqliteValue::Integer(1)]
        );
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(
            parse_record(&cursor.payload(&cx).unwrap()).unwrap(),
            vec![SqliteValue::Integer(20), SqliteValue::Integer(2)]
        );
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(
            parse_record(&cursor.payload(&cx).unwrap()).unwrap(),
            vec![SqliteValue::Integer(30), SqliteValue::Integer(3)]
        );
    }

    #[test]
    fn test_index_insert_unique_non_leaf_restore_state_falls_back_to_full_insert() {
        let low_key = serialize_record(&[
            SqliteValue::Text("alpha@example.com".into()),
            SqliteValue::Integer(1),
        ]);
        let separator_key = serialize_record(&[
            SqliteValue::Text("mango@example.com".into()),
            SqliteValue::Integer(2),
        ]);
        let high_key = serialize_record(&[
            SqliteValue::Text("zebra@example.com".into()),
            SqliteValue::Integer(3),
        ]);
        let probe_key = serialize_record(&[
            SqliteValue::Text("hotel@example.com".into()),
            SqliteValue::Integer(4),
        ]);

        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_index(&[(pn(3), &separator_key)], pn(4)));
        store.pages.insert(3, build_leaf_index(&[&low_key]));
        store.pages.insert(4, build_leaf_index(&[&high_key]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);

        let restores_to_non_leaf = cursor
            .with_btree_op(&cx, BtreeOpType::Seek, |cursor| {
                let _seek = cursor.index_seek(&cx, &probe_key)?;
                let restore_eof = cursor.at_eof;

                if !cursor.at_eof {
                    let _payload = cursor.payload(&cx)?;
                }

                if cursor.prev(&cx)? {
                    let _payload = cursor.payload(&cx)?;
                }

                if restore_eof {
                    cursor.at_eof = true;
                } else {
                    cursor.next(&cx)?;
                }

                Ok(cursor
                    .stack
                    .last()
                    .is_some_and(|top| !top.header.page_type.is_leaf()))
            })
            .unwrap();

        assert!(
            restores_to_non_leaf,
            "test requires the uniqueness restore state to sit on an interior separator"
        );

        cursor
            .index_insert_unique(&cx, &probe_key, 1, "bench.email")
            .unwrap();

        assert!(cursor.index_move_to(&cx, &probe_key).unwrap().is_found());
        assert_eq!(
            parse_record(&cursor.payload(&cx).unwrap()).unwrap(),
            vec![
                SqliteValue::Text("hotel@example.com".into()),
                SqliteValue::Integer(4),
            ]
        );
    }

    #[test]
    fn test_index_insert_monotonic_unique_email_keys_10k_counts_and_reaches_last_key() {
        let cx = Cx::new();
        let root = pn(2);
        let store = MemPageStore::with_empty_index(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, false);
        let row_count = 10_000_i64;

        for rowid in 1..=row_count {
            let key = serialize_record(&[
                SqliteValue::Text(format!("user_{:05}@test.com", rowid - 1).into()),
                SqliteValue::Integer(rowid),
            ]);
            cursor.index_insert(&cx, &key).unwrap();
        }

        let depth = measure_tree_depth(&cursor.pager, root, USABLE);
        assert!(
            depth >= 2,
            "test requires at least one interior index level, got depth {depth}"
        );

        let last_key = serialize_record(&[
            SqliteValue::Text(format!("user_{:05}@test.com", row_count - 1).into()),
            SqliteValue::Integer(row_count),
        ]);
        let last_found = cursor.index_move_to(&cx, &last_key).unwrap().is_found();
        let last_payload = last_found.then(|| parse_record(&cursor.payload(&cx).unwrap()).unwrap());
        let count = cursor.count_all_rows(&cx).unwrap();
        assert_eq!(
            count, row_count,
            "plain monotonic index_insert should preserve all {row_count} index entries; last_found={last_found} last_payload={last_payload:?}"
        );

        assert!(
            last_found,
            "plain monotonic index_insert should keep the last key reachable"
        );
        assert_eq!(
            last_payload.unwrap(),
            vec![
                SqliteValue::Text(format!("user_{:05}@test.com", row_count - 1).into()),
                SqliteValue::Integer(row_count),
            ]
        );
    }

    #[test]
    fn test_index_insert_unique_monotonic_unique_email_keys_10k_counts_and_reaches_last_key() {
        let cx = Cx::new();
        let root = pn(2);
        let store = MemPageStore::with_empty_index(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, false);
        let row_count = 10_000_i64;

        for rowid in 1..=row_count {
            let key = serialize_record(&[
                SqliteValue::Text(format!("user_{:05}@test.com", rowid - 1).into()),
                SqliteValue::Integer(rowid),
            ]);
            cursor
                .index_insert_unique(&cx, &key, 1, "bench.email")
                .unwrap();
        }

        let depth = measure_tree_depth(&cursor.pager, root, USABLE);
        assert!(
            depth >= 2,
            "test requires at least one interior index level, got depth {depth}"
        );

        let last_key = serialize_record(&[
            SqliteValue::Text(format!("user_{:05}@test.com", row_count - 1).into()),
            SqliteValue::Integer(row_count),
        ]);
        let last_found = cursor.index_move_to(&cx, &last_key).unwrap().is_found();
        let last_payload = last_found.then(|| parse_record(&cursor.payload(&cx).unwrap()).unwrap());
        let count = cursor.count_all_rows(&cx).unwrap();
        assert_eq!(
            count, row_count,
            "monotonic unique index_insert_unique should preserve all {row_count} index entries; last_found={last_found} last_payload={last_payload:?}"
        );

        assert!(
            last_found,
            "monotonic unique index_insert_unique should keep the last key reachable"
        );
        assert_eq!(
            last_payload.unwrap(),
            vec![
                SqliteValue::Text(format!("user_{:05}@test.com", row_count - 1).into()),
                SqliteValue::Integer(row_count),
            ]
        );
    }

    #[test]
    fn test_index_insert_unique_deep_tree_monotonic_10k_counts_all_rows() {
        let store = MemPageStore::with_empty_index(pn(2), USABLE);
        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);
        let row_count = 10_000_i64;

        for rowid in 0..row_count {
            let key = serialize_record(&[
                SqliteValue::Text(format!("user_{rowid}@test.com").into()),
                SqliteValue::Integer(rowid),
            ]);
            cursor
                .index_insert_unique(&cx, &key, 1, "bench.email")
                .unwrap();
        }

        let counted = cursor.count_all_rows(&cx).unwrap();
        assert_eq!(
            counted, row_count,
            "deep monotonic unique index should contain every inserted entry"
        );

        let last_key = serialize_record(&[
            SqliteValue::Text(format!("user_{}@test.com", row_count - 1).into()),
            SqliteValue::Integer(row_count - 1),
        ]);
        let seek = cursor.index_move_to(&cx, &last_key).unwrap();
        assert_eq!(seek, SeekResult::Found);
        assert_eq!(cursor.payload(&cx).unwrap(), last_key);
    }

    #[test]
    fn test_index_insert_deep_tree_monotonic_10k_counts_all_rows() {
        let store = MemPageStore::with_empty_index(pn(2), USABLE);
        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);
        let row_count = 10_000_i64;

        for rowid in 0..row_count {
            let key = serialize_record(&[
                SqliteValue::Text(format!("user_{rowid}@test.com").into()),
                SqliteValue::Integer(rowid),
            ]);
            cursor.index_insert(&cx, &key).unwrap();
        }

        let counted = cursor.count_all_rows(&cx).unwrap();
        assert_eq!(
            counted, row_count,
            "deep monotonic plain index should contain every inserted entry"
        );

        let last_key = serialize_record(&[
            SqliteValue::Text(format!("user_{}@test.com", row_count - 1).into()),
            SqliteValue::Integer(row_count - 1),
        ]);
        let seek = cursor.index_move_to(&cx, &last_key).unwrap();
        assert_eq!(seek, SeekResult::Found);
        assert_eq!(cursor.payload(&cx).unwrap(), last_key);
    }

    #[test]
    fn test_cursor_index_next_visits_interior_separator_cells() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_interior_index(&[(pn(3), b"b"), (pn(4), b"d")], pn(5)),
        );
        store.pages.insert(3, build_leaf_index(&[b"a"]));
        store.pages.insert(4, build_leaf_index(&[b"c"]));
        store.pages.insert(5, build_leaf_index(&[b"e", b"f"]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, false);
        assert!(cursor.first(&cx).unwrap());
        let mut scanned = vec![cursor.payload(&cx).unwrap()];
        while cursor.next(&cx).unwrap() {
            scanned.push(cursor.payload(&cx).unwrap());
        }
        assert_eq!(
            scanned,
            vec![
                b"a".to_vec(),
                b"b".to_vec(),
                b"c".to_vec(),
                b"d".to_vec(),
                b"e".to_vec(),
                b"f".to_vec(),
            ]
        );
    }

    #[test]
    fn test_cursor_index_prev_visits_interior_separator_cells() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_interior_index(&[(pn(3), b"b"), (pn(4), b"d")], pn(5)),
        );
        store.pages.insert(3, build_leaf_index(&[b"a"]));
        store.pages.insert(4, build_leaf_index(&[b"c"]));
        store.pages.insert(5, build_leaf_index(&[b"e", b"f"]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);

        assert!(cursor.last(&cx).unwrap());
        let mut scanned = vec![cursor.payload(&cx).unwrap()];
        while cursor.prev(&cx).unwrap() {
            scanned.push(cursor.payload(&cx).unwrap());
        }
        assert_eq!(
            scanned,
            vec![
                b"f".to_vec(),
                b"e".to_vec(),
                b"d".to_vec(),
                b"c".to_vec(),
                b"b".to_vec(),
                b"a".to_vec(),
            ]
        );
    }

    #[test]
    fn test_count_all_rows_on_interior_index_includes_separator_cells() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_interior_index(&[(pn(3), b"b"), (pn(4), b"d")], pn(5)),
        );
        store.pages.insert(3, build_leaf_index(&[b"a"]));
        store.pages.insert(4, build_leaf_index(&[b"c"]));
        store.pages.insert(5, build_leaf_index(&[b"e", b"f"]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);
        assert_eq!(cursor.count_all_rows(&cx).unwrap(), 6);
    }

    #[test]
    fn test_cursor_index_rowid_extracted_from_trailing_record_field() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_index(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let key = serialize_record(&[SqliteValue::Text("beacon".into()), SqliteValue::Integer(73)]);

        cursor.index_insert(&cx, &key).unwrap();
        assert!(cursor.index_move_to(&cx, &key).unwrap().is_found());
        assert_eq!(cursor.rowid(&cx).unwrap(), 73);
    }

    #[test]
    fn test_cursor_index_rowid_with_overflow_key_payload() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_index(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let key = serialize_record(&[
            SqliteValue::Blob(vec![0xAB; 2_500].into()),
            SqliteValue::Integer(901),
        ]);

        cursor.index_insert(&cx, &key).unwrap();
        assert!(cursor.index_move_to(&cx, &key).unwrap().is_found());
        assert_eq!(cursor.rowid(&cx).unwrap(), 901);
    }

    #[test]
    fn test_cursor_index_seek_duplicate_run_walks_all_matching_entries() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_index(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);

        for rowid in [1_i64, 2, 5] {
            let key = serialize_record(&[SqliteValue::Integer(42), SqliteValue::Integer(rowid)]);
            cursor.index_insert(&cx, &key).unwrap();
        }
        let other_key = serialize_record(&[SqliteValue::Integer(99), SqliteValue::Integer(9)]);

        cursor.index_insert(&cx, &other_key).unwrap();

        let probe = serialize_record(&[SqliteValue::Integer(42), SqliteValue::Integer(i64::MIN)]);
        let seek = cursor.index_move_to(&cx, &probe).unwrap();
        assert!(
            !seek.is_found(),
            "probe uses a synthetic minimum rowid and should anchor via successor positioning"
        );
        assert!(
            !cursor.eof(),
            "duplicate-run probe should land on the first matching entry"
        );

        let mut seen_rowids = Vec::new();
        loop {
            let payload = cursor.payload(&cx).unwrap();
            let fields = parse_record(&payload).unwrap();
            if fields.first() != Some(&SqliteValue::Integer(42)) {
                break;
            }
            seen_rowids.push(cursor.rowid(&cx).unwrap());
            if !cursor.next(&cx).unwrap() {
                break;
            }
        }

        assert_eq!(seen_rowids, vec![1, 2, 5]);
    }

    #[test]
    fn test_cursor_index_rowid_rejects_record_without_trailing_integer() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_index(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);

        let key = serialize_record(&[SqliteValue::Text("missing-rowid".into())]);

        cursor.index_insert(&cx, &key).unwrap();
        assert!(cursor.index_move_to(&cx, &key).unwrap().is_found());

        let err = cursor.rowid(&cx).unwrap_err();
        assert!(matches!(err, FrankenError::DatabaseCorrupt { .. }));
    }

    #[test]
    fn test_cursor_table_insert_with_overflow_payload() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));
        let payload: Vec<u8> = (0u8..=255).cycle().take(5000).collect();

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        cursor.table_insert(&cx, 42, &payload).unwrap();

        assert!(cursor.table_move_to(&cx, 42).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), payload);
    }

    #[test]
    fn test_cursor_table_seek_past_end_then_insert() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (2, b"two"), (3, b"three"), (4, b"four")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let seek = cursor.table_move_to(&cx, 99).unwrap();
        assert!(!seek.is_found());
        assert!(cursor.eof(), "seek past end should set eof");

        cursor.table_insert(&cx, 99, b"tail").unwrap();
        assert!(cursor.table_move_to(&cx, 99).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"tail");
    }

    #[test]
    fn test_cursor_index_seek_past_end_then_insert() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_index(&[b"alpha", b"mid"]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);
        let key = b"zz-top";

        let seek = cursor.index_move_to(&cx, key).unwrap();
        assert!(!seek.is_found());
        assert!(cursor.eof(), "seek past end should set eof");

        cursor.index_insert(&cx, key).unwrap();
        assert!(cursor.index_move_to(&cx, key).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), key);
    }

    #[test]
    fn test_cursor_prev_from_seek_past_end_lands_on_last_entry() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (2, b"two"), (3, b"three"), (4, b"four")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let seek = cursor.table_move_to(&cx, 99).unwrap();
        assert!(!seek.is_found());
        assert!(cursor.eof());

        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 4);
    }

    #[test]
    fn test_cursor_next_after_prev_from_first_recovers() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (2, b"two"), (3, b"three"), (4, b"four")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(!cursor.prev(&cx).unwrap());

        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 2);
    }

    #[test]
    fn test_cursor_prev_after_next_from_last_recovers() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (2, b"two"), (3, b"three"), (4, b"four")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, true);

        assert!(cursor.last(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 4);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 3);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 2);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(!cursor.prev(&cx).unwrap());
    }

    #[test]
    fn test_cursor_table_insert_overflow_write_failure_frees_allocations() {
        let cx = Cx::new();
        let root_page = pn(2);
        let mut base = MemPageStore::new(USABLE);
        base.init_leaf_table_root(root_page);
        let shared = Rc::new(RefCell::new(base));

        // Force a mid-chain overflow write failure.
        let failing = FailingOverflowStore::new(Rc::clone(&shared), 2);
        let mut cursor = BtCursor::new(failing, root_page, USABLE, true);
        let payload = vec![0xCC; 9_000];

        let err = cursor.table_insert(&cx, 1, &payload).unwrap_err();
        assert!(matches!(err, FrankenError::Internal(_)));
        assert_eq!(
            shared.borrow().pages.len(),
            1,
            "only the root page should remain after failed overflow write"
        );
    }

    #[test]
    fn test_cursor_delete_masks_overflow_cleanup_cancellation() {
        let root_page = pn(2);
        let mut base = MemPageStore::new(USABLE);
        base.init_leaf_table_root(root_page);
        let shared = Rc::new(RefCell::new(base));

        let store = CancelAfterFirstOverflowFreeStore::new(Rc::clone(&shared));
        let mut cursor = BtCursor::new(store, root_page, USABLE, true);

        let insert_cx = Cx::new();
        let payload = vec![0xAB; 9_000];
        cursor.table_insert(&insert_cx, 7, &payload).unwrap();

        let pages_before: BTreeSet<u32> = shared.borrow().pages.keys().copied().collect();
        assert!(
            pages_before.len() > 2,
            "test requires a multi-page overflow chain, found pages {pages_before:?}"
        );

        let delete_cx = Cx::new();
        assert!(cursor.table_move_to(&delete_cx, 7).unwrap().is_found());
        cursor
            .delete(&delete_cx)
            .expect("overflow cleanup must finish even if cancellation arrives mid-chain");
        assert!(
            delete_cx.checkpoint().is_err(),
            "test store should request cancellation during overflow cleanup"
        );

        let recovery_cx = Cx::new();
        assert!(!cursor.first(&recovery_cx).unwrap());

        let remaining_pages: BTreeSet<u32> = shared.borrow().pages.keys().copied().collect();
        assert_eq!(
            remaining_pages,
            BTreeSet::from([root_page.get()]),
            "masked cleanup must reclaim every overflow page before returning"
        );
    }

    #[test]
    fn test_cursor_table_insert_triggers_root_split() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let mut rowid = 1i64;
        let split_rowid = loop {
            let payload = vec![b'Z'; 220];
            cursor.table_insert(&cx, rowid, &payload).unwrap();

            let root_page = cursor.pager.inner.pages.get(&2).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorTable {
                break rowid;
            }

            rowid += 1;
            assert!(
                rowid < 1000,
                "table root did not split under sustained inserts"
            );
        };

        let root_page = cursor.pager.inner.pages.get(&2).unwrap();
        let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
        assert_eq!(root_header.page_type, cell::BtreePageType::InteriorTable);

        assert!(cursor.table_move_to(&cx, split_rowid).unwrap().is_found());
    }

    #[test]
    fn test_cursor_index_insert_triggers_root_split() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_index(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, false);
        let mut idx = 0usize;
        let split_key = loop {
            let key = format!("key-{idx:05}");
            cursor.index_insert(&cx, key.as_bytes()).unwrap();

            let root_page = cursor.pager.pages.get(&2).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorIndex {
                break key.into_bytes();
            }

            idx += 1;
            assert!(
                idx < 2000,
                "index root did not split under sustained inserts"
            );
        };

        let root_page = cursor.pager.pages.get(&2).unwrap();
        let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
        assert_eq!(root_header.page_type, cell::BtreePageType::InteriorIndex);

        assert!(cursor.index_move_to(&cx, &split_key).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), split_key);
    }

    #[test]
    fn test_cursor_index_delete_removes_interior_separator_key() {
        const INDEX_USABLE: u32 = 512;

        let root = pn(2);
        let store = MemPageStore::with_empty_index(root, INDEX_USABLE);
        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, root, INDEX_USABLE, false);

        let mut key_idx = 0usize;
        let separator_key = loop {
            let key = format!("key-{key_idx:04}").into_bytes();
            cursor.index_insert(&cx, &key).unwrap();
            key_idx += 1;

            let root_page = cursor.pager.pages.get(&root.get()).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorIndex {
                let root_entry = cursor.load_page(&cx, root).unwrap();
                let divider = cursor.parse_cell_at(&root_entry, 0).unwrap();
                break cursor
                    .read_cell_payload(&cx, &root_entry, &divider)
                    .unwrap()
                    .into_owned();
            }
        };

        let mut expected = Vec::new();
        if cursor.first(&cx).unwrap() {
            loop {
                expected.push(cursor.payload(&cx).unwrap());
                if !cursor.next(&cx).unwrap() {
                    break;
                }
            }
        }
        expected.retain(|key| key.as_slice() != separator_key.as_slice());

        let seek = cursor.index_move_to(&cx, &separator_key).unwrap();
        assert!(
            seek.is_found(),
            "separator key should be seekable before delete"
        );
        assert!(
            !cursor
                .stack
                .last()
                .expect("separator seek should leave a cursor frame")
                .header
                .page_type
                .is_leaf(),
            "separator key must resolve to an interior frame to exercise interior delete"
        );

        cursor.delete(&cx).unwrap();

        let seek_after = cursor.index_move_to(&cx, &separator_key).unwrap();
        assert!(
            !seek_after.is_found(),
            "deleted separator key must not remain reachable"
        );

        let mut scanned = Vec::new();
        if cursor.first(&cx).unwrap() {
            loop {
                scanned.push(cursor.payload(&cx).unwrap());
                if !cursor.next(&cx).unwrap() {
                    break;
                }
            }
        }
        assert_eq!(
            scanned, expected,
            "interior delete must remove the separator without leaving a stale logical entry"
        );
    }

    #[test]
    fn test_cursor_repeated_root_overflow_does_not_leave_orphan_pages() {
        const SMALL_USABLE: u32 = 512;

        let root = pn(2);
        let store = MemPageStore::with_empty_table(root, SMALL_USABLE);
        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, root, SMALL_USABLE, true);

        for rowid in 1_i64..=200_i64 {
            cursor.table_insert(&cx, rowid, &vec![b'R'; 180]).unwrap();
        }

        let root_page = cursor.pager.pages.get(&root.get()).unwrap();
        let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
        assert!(
            root_header.page_type.is_interior(),
            "test requires an interior root after sustained inserts"
        );

        let all_pages: BTreeSet<u32> = cursor.pager.pages.keys().copied().collect();
        assert!(
            all_pages.len() > 6,
            "test requires enough pages to exercise repeated root overflow"
        );

        let mut reachable = BTreeSet::new();
        collect_reachable_pages(&cursor.pager, root, SMALL_USABLE, &mut reachable);
        assert_eq!(
            reachable, all_pages,
            "repeated root overflow must not leave detached child generations behind"
        );
    }

    #[test]
    fn test_cursor_table_insert_after_root_split() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let mut rowid = 1i64;
        loop {
            let payload = vec![b'Z'; 220];
            cursor.table_insert(&cx, rowid, &payload).unwrap();
            let root_page = cursor.pager.inner.pages.get(&2).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorTable {
                break;
            }
            rowid += 1;
            assert!(
                rowid < 1000,
                "table root did not split under sustained inserts"
            );
        }

        // Insert after split to exercise multi-level insert path.
        rowid += 1;
        cursor.table_insert(&cx, rowid, b"after-split").unwrap();

        assert!(cursor.table_move_to(&cx, rowid).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"after-split");
    }

    #[test]
    fn test_cursor_delete_single_leaf() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (2, b"two"), (3, b"three")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        assert!(cursor.table_move_to(&cx, 2).unwrap().is_found());
        cursor.delete(&cx).unwrap();

        let result = cursor.table_move_to(&cx, 2).unwrap();
        assert!(!result.is_found());
        assert!(!cursor.eof());
        assert_eq!(cursor.rowid(&cx).unwrap(), 3);

        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 3);
        assert!(!cursor.next(&cx).unwrap());
    }

    #[test]
    fn test_cursor_delete_after_root_split() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let mut max_rowid = 0i64;
        loop {
            let payload = vec![b'Q'; 220];
            cursor.table_insert(&cx, max_rowid, &payload).unwrap();
            let root_page = cursor.pager.inner.pages.get(&2).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorTable {
                break;
            }
            max_rowid += 1;
            assert!(
                max_rowid < 1000,
                "table root did not split under sustained inserts"
            );
        }

        let root_page = cursor.pager.inner.pages.get(&2).unwrap();
        let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
        let root_ptrs = cell::read_cell_pointers(root_page, &root_header, 0).unwrap();
        let first_divider_cell = CellRef::parse(
            root_page,
            usize::from(root_ptrs[0]),
            root_header.page_type,
            USABLE,
        )
        .unwrap();
        let leftmost_max_rowid = first_divider_cell.rowid.unwrap();
        assert!(leftmost_max_rowid >= 1);
        assert!(leftmost_max_rowid < max_rowid);

        for rowid in 0..=leftmost_max_rowid {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} should exist before delete");
            cursor.delete(&cx).unwrap();
        }

        // After balance_shallower, the root collapses from interior
        // (with 0 cells and a single right-child) down to whatever page
        // type the right-child was.  For a depth-2 tree the right-child
        // is a leaf, so the root becomes a leaf.
        let root_data = cursor.pager.read_page(&cx, pn(2)).unwrap();
        let root_header = BtreePageHeader::parse(&root_data, 0).unwrap();
        assert!(
            root_header.page_type.is_leaf(),
            "root should collapse to leaf after all rows deleted, got {:?}",
            root_header.page_type
        );
        assert_eq!(
            root_header.cell_count, 4,
            "root collapse should preserve the surviving leaf rows"
        );
    }

    #[test]
    fn test_cursor_delete_rebalances_empty_leftmost_leaf() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let mut max_rowid = 0i64;
        loop {
            let payload = vec![b'P'; 220];
            cursor.table_insert(&cx, max_rowid, &payload).unwrap();
            let root_page = cursor.pager.inner.pages.get(&2).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorTable {
                break;
            }
            max_rowid += 1;
            assert!(
                max_rowid < 1000,
                "table root did not split under sustained inserts"
            );
        }

        let root_page = cursor.pager.inner.pages.get(&2).unwrap();
        let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
        let root_ptrs = cell::read_cell_pointers(root_page, &root_header, 0).unwrap();
        let first_divider_cell = CellRef::parse(
            root_page,
            usize::from(root_ptrs[0]),
            root_header.page_type,
            USABLE,
        )
        .unwrap();
        let leftmost_max_rowid = first_divider_cell.rowid.unwrap();
        assert!(leftmost_max_rowid >= 1);
        assert!(leftmost_max_rowid < max_rowid);

        for rowid in 0..=leftmost_max_rowid {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} should exist before delete");
            cursor.delete(&cx).unwrap();
        }

        // After balance_shallower, the root collapses from interior
        // (with 0 cells and a single right-child) down to whatever page
        // type the right-child was.  For a depth-2 tree the right-child
        // is a leaf, so the root becomes a leaf.
        let root_data = cursor.pager.read_page(&cx, pn(2)).unwrap();
        let root_header = BtreePageHeader::parse(&root_data, 0).unwrap();
        assert!(
            root_header.page_type.is_leaf(),
            "root should collapse to leaf after all rows deleted, got {:?}",
            root_header.page_type
        );
        assert_eq!(
            root_header.cell_count, 4,
            "root collapse should preserve the surviving leaf rows"
        );
    }

    #[test]
    fn test_cursor_delete_all_after_root_split() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let mut max_rowid = 0i64;
        loop {
            let payload = vec![b'Q'; 220];
            cursor.table_insert(&cx, max_rowid, &payload).unwrap();
            let root_page = cursor.pager.inner.pages.get(&2).unwrap();
            let root_header = BtreePageHeader::parse(root_page, 0).unwrap();
            if root_header.page_type == cell::BtreePageType::InteriorTable {
                break;
            }
            max_rowid += 1;
            assert!(
                max_rowid < 1000,
                "table root did not split under sustained inserts"
            );
        }

        for rowid in 0..=max_rowid {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} should exist before delete");
            cursor.delete(&cx).unwrap();
        }

        assert!(
            !cursor.first(&cx).unwrap(),
            "tree should be empty after total delete"
        );
        assert!(cursor.eof());

        // The root page should have collapsed to a leaf (depth 1).
        let root_data = cursor.pager.read_page(&cx, pn(2)).unwrap();
        let root_header = BtreePageHeader::parse(&root_data, 0).unwrap();
        assert!(
            root_header.page_type.is_leaf(),
            "root should collapse to leaf after all rows deleted, got {:?}",
            root_header.page_type
        );
        assert_eq!(root_header.cell_count, 0);
    }

    #[test]
    fn test_e2e_bd_2kvo() {
        const TOTAL_ROWS: i64 = 2_000;
        const DELETE_ROWS: usize = 1_000;

        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        let mut expected = BTreeMap::<i64, Vec<u8>>::new();

        for rowid in 1..=TOTAL_ROWS {
            let payload = payload_for_rowid(rowid);
            cursor.table_insert(&cx, rowid, &payload).unwrap();
            expected.insert(rowid, payload);
        }

        for (rowid, payload) in &expected {
            let seek = cursor.table_move_to(&cx, *rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} not found");
            let got = cursor.payload(&cx).unwrap();
            assert_eq!(
                got.len(),
                payload.len(),
                "payload length mismatch at rowid {rowid}"
            );
            assert_eq!(&got[..], &payload[..], "payload mismatch at rowid {rowid}");
        }

        let mut deletion_order: Vec<i64> = expected.keys().copied().collect();
        deterministic_shuffle(&mut deletion_order, 0x0BAD_5EED);

        for rowid in deletion_order.into_iter().take(DELETE_ROWS) {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} should exist before delete");
            cursor.delete(&cx).unwrap();
            expected.remove(&rowid);
        }

        if expected.is_empty() {
            assert!(!cursor.first(&cx).unwrap());
            assert!(cursor.eof());
            return;
        }

        let mut expected_iter = expected.iter();
        assert!(cursor.first(&cx).unwrap());
        loop {
            let rowid = cursor.rowid(&cx).unwrap();
            let payload = cursor.payload(&cx).unwrap();

            let (expected_rowid, expected_payload) =
                expected_iter.next().expect("cursor yielded extra row");
            assert_eq!(rowid, *expected_rowid);
            assert_eq!(payload.len(), expected_payload.len());
            assert_eq!(&payload[..], expected_payload);

            if !cursor.next(&cx).unwrap() {
                break;
            }
        }

        assert!(
            expected_iter.next().is_none(),
            "cursor missed one or more rows during forward scan"
        );
    }

    #[test]
    fn test_e2e_btree_prefetch_latency() {
        const TOTAL_ROWS: i64 = 1_500;

        let mut seed_store = MemPageStore::new(USABLE);
        seed_store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut seed_cursor = BtCursor::new(seed_store, pn(2), USABLE, true);
        for rowid in 1..=TOTAL_ROWS {
            let payload = payload_for_rowid(rowid);
            seed_cursor.table_insert(&cx, rowid, &payload).unwrap();
        }

        let baseline_store = seed_cursor.pager.clone();
        let prefetch_store = PrefetchProbeStore::new(seed_cursor.pager);

        let mut workload: Vec<i64> = (1..=TOTAL_ROWS).collect();
        deterministic_shuffle(&mut workload, 0x0FEE_D123);

        let mut baseline_cursor = BtCursor::new(baseline_store, pn(2), USABLE, true);
        let baseline_started = Instant::now();
        let mut baseline_total_bytes = 0usize;
        for rowid in &workload {
            let result = baseline_cursor.table_move_to(&cx, *rowid).unwrap();
            assert!(result.is_found(), "baseline lookup miss for rowid={rowid}");
            baseline_total_bytes += baseline_cursor.payload(&cx).unwrap().len();
        }
        let baseline_elapsed = baseline_started.elapsed();

        let mut hinted_cursor = BtCursor::new(prefetch_store, pn(2), USABLE, true);
        let hinted_started = Instant::now();
        let mut hinted_total_bytes = 0usize;
        for rowid in &workload {
            let result = hinted_cursor.table_move_to(&cx, *rowid).unwrap();
            assert!(result.is_found(), "hinted lookup miss for rowid={rowid}");
            hinted_total_bytes += hinted_cursor.payload(&cx).unwrap().len();
        }
        let hinted_elapsed = hinted_started.elapsed();

        assert_eq!(baseline_total_bytes, hinted_total_bytes);
        assert!(
            !hinted_cursor.pager.hinted_pages().is_empty(),
            "prefetch implementation must remain fully safe"
        );

        let allowed_regression = baseline_elapsed.saturating_mul(50) + Duration::from_millis(250);
        assert!(
            hinted_elapsed <= allowed_regression,
            "prefetch workload regressed too much: baseline={baseline_elapsed:?}, hinted={hinted_elapsed:?}"
        );
    }

    #[test]
    fn test_btree_insert_delete_5k() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        let mut remaining = BTreeSet::new();

        // Insert 10,000 rows so that deleting 5,000 leaves 5,000 survivors.
        for i in 1..=10_000_i64 {
            let payload = payload_for_rowid(i);
            cursor.table_insert(&cx, i, &payload).unwrap();
            remaining.insert(i);
        }

        let mut deletion_order: Vec<i64> = remaining.iter().copied().collect();
        deterministic_shuffle(&mut deletion_order, 0x00D1_5EA5);

        for rowid in deletion_order.into_iter().take(5_000) {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} should exist before delete");
            cursor.delete(&cx).unwrap();
            remaining.remove(&rowid);
        }

        assert_eq!(remaining.len(), 5_000);
        assert!(cursor.first(&cx).unwrap());

        let mut expected_iter = remaining.iter();
        loop {
            let rowid = cursor.rowid(&cx).unwrap();
            let expected = expected_iter.next().expect("cursor yielded extra row");
            assert_eq!(&rowid, expected);

            if !cursor.next(&cx).unwrap() {
                break;
            }
        }

        assert!(
            expected_iter.next().is_none(),
            "cursor missed one or more rows after delete workload"
        );
    }

    #[test]
    fn test_btree_insert_delete_sorted_order() {
        test_btree_insert_delete_5k();
    }

    #[test]
    fn test_table_insert_rightmost_hint_appends_and_falls_back_for_midstream_key() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, true);
        cursor
            .table_insert_rightmost_hint(&cx, 1, payload_for_rowid(1).as_slice())
            .unwrap();
        cursor
            .table_insert_rightmost_hint(&cx, 3, payload_for_rowid(3).as_slice())
            .unwrap();
        cursor
            .table_insert_rightmost_hint(&cx, 2, payload_for_rowid(2).as_slice())
            .unwrap();

        for rowid in 4..=256_i64 {
            let payload = payload_for_rowid(rowid);
            cursor
                .table_insert_rightmost_hint(&cx, rowid, payload.as_slice())
                .unwrap();
        }

        assert!(cursor.first(&cx).unwrap());
        for expected_rowid in 1..=256_i64 {
            assert_eq!(cursor.rowid(&cx).unwrap(), expected_rowid);
            if expected_rowid < 256 {
                assert!(cursor.next(&cx).unwrap());
            }
        }
        assert!(!cursor.next(&cx).unwrap());
    }

    #[test]
    fn test_table_insert_rightmost_leaf_hint_reuses_leaf_and_falls_back_after_split() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, true);
        cursor
            .table_insert_rightmost_hint(&cx, 1, payload_for_rowid(1).as_slice())
            .unwrap();

        let mut hinted_leaf = cursor
            .current_page()
            .expect("first append should leave the cursor on a concrete leaf");
        for rowid in 2..=256_i64 {
            let payload = payload_for_rowid(rowid);
            cursor
                .table_insert_rightmost_leaf_hint(&cx, hinted_leaf, rowid, payload.as_slice())
                .unwrap();
            if let Some(current_leaf) = cursor.current_page() {
                hinted_leaf = current_leaf;
            }
        }

        assert!(cursor.first(&cx).unwrap());
        for expected_rowid in 1..=256_i64 {
            assert_eq!(cursor.rowid(&cx).unwrap(), expected_rowid);
            if expected_rowid < 256 {
                assert!(cursor.next(&cx).unwrap());
            }
        }
        assert!(!cursor.next(&cx).unwrap());
    }

    #[test]
    fn test_btree_insert_10k_random_keys() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        let mut insertion_order: Vec<i64> = (1_i64..=10_000_i64).collect();
        deterministic_shuffle(&mut insertion_order, 0x000D_EADB);

        for rowid in insertion_order {
            let payload = payload_for_rowid(rowid);
            cursor.table_insert(&cx, rowid, &payload).unwrap();
        }

        for rowid in 1_i64..=10_000_i64 {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "missing rowid {rowid} after insert");
            assert_eq!(cursor.rowid(&cx).unwrap(), rowid);
            assert_eq!(cursor.payload(&cx).unwrap(), payload_for_rowid(rowid));
        }
    }

    #[test]
    fn test_btree_depth_4_cursor_traversal() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 100)], pn(7)));
        store
            .pages
            .insert(3, build_interior_table(&[(pn(4), 50)], pn(8)));
        store
            .pages
            .insert(4, build_interior_table(&[(pn(5), 25)], pn(6)));
        store
            .pages
            .insert(5, build_leaf_table(&[(10, b"ten"), (20, b"twenty")]));
        store
            .pages
            .insert(6, build_leaf_table(&[(30, b"thirty"), (40, b"forty")]));
        store
            .pages
            .insert(8, build_leaf_table(&[(60, b"sixty"), (80, b"eighty")]));
        store
            .pages
            .insert(7, build_leaf_table(&[(120, b"one20"), (140, b"one40")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        let depth = measure_tree_depth(&cursor.pager, pn(2), USABLE);
        assert_eq!(depth, 4, "expected a manually seeded depth-4 tree");

        let expected_rowids = [10_i64, 20, 30, 40, 60, 80, 120, 140];
        for rowid in expected_rowids {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "missing rowid {rowid} in depth-4 tree");
        }

        assert!(cursor.first(&cx).unwrap());
        let mut scanned = vec![cursor.rowid(&cx).unwrap()];
        while cursor.next(&cx).unwrap() {
            scanned.push(cursor.rowid(&cx).unwrap());
        }
        assert_eq!(scanned, expected_rowids);
    }

    #[test]
    fn test_point_read_uses_cell_witness() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (5, b"five"), (10, b"ten")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let result = cursor.table_move_to(&cx, 5).unwrap();
        assert!(result.is_found());
        assert_eq!(cursor.witness_keys().len(), 1);
        assert!(matches!(cursor.witness_keys()[0], WitnessKey::Cell { .. }));
    }

    #[test]
    fn test_descent_pages_not_witnessed() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 5)], pn(4)));
        store
            .pages
            .insert(3, build_leaf_table(&[(1, b"a"), (2, b"b")]));
        store
            .pages
            .insert(4, build_leaf_table(&[(10, b"c"), (15, b"d")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let result = cursor.table_move_to(&cx, 10).unwrap();
        assert!(result.is_found());
        assert!(
            cursor
                .witness_keys()
                .iter()
                .all(|key| matches!(key, WitnessKey::Cell { .. }))
        );
    }

    #[test]
    fn test_negative_read_uses_cell_witness() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"one"), (5, b"five"), (10, b"ten")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let result = cursor.table_move_to(&cx, 7).unwrap();
        assert!(!result.is_found());
        assert_eq!(cursor.witness_keys().len(), 1);
        assert!(matches!(cursor.witness_keys()[0], WitnessKey::Cell { .. }));
    }

    #[test]
    fn test_range_scan_uses_page_witness() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 5)], pn(4)));
        store
            .pages
            .insert(3, build_leaf_table(&[(1, b"a"), (5, b"b")]));
        store
            .pages
            .insert(4, build_leaf_table(&[(10, b"c"), (15, b"d")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(store, pn(2), USABLE, true);

        assert!(cursor.first(&cx).unwrap());
        assert!(cursor.next(&cx).unwrap());
        assert!(cursor.next(&cx).unwrap());
        assert!(
            cursor
                .witness_keys()
                .iter()
                .any(|key| matches!(key, WitnessKey::Page(_)))
        );
    }

    #[test]
    fn test_page_only_witnesses_collapse_merge() {
        use std::collections::HashSet;

        let root = pn(2);
        let same_leaf = pn(4);

        let txn1_cell = HashSet::from([WitnessKey::Cell {
            btree_root: root,
            leaf_page: same_leaf,
            tag: BtCursor::<MemPageStore>::cell_tag_from_rowid(10),
        }]);
        let txn2_cell = HashSet::from([WitnessKey::Cell {
            btree_root: root,
            leaf_page: same_leaf,
            tag: BtCursor::<MemPageStore>::cell_tag_from_rowid(11),
        }]);
        assert!(
            txn1_cell.is_disjoint(&txn2_cell),
            "cell witnesses preserve independent point operations"
        );

        let txn1_page = HashSet::from([WitnessKey::Page(same_leaf)]);
        let txn2_page = HashSet::from([WitnessKey::Page(same_leaf)]);
        assert!(
            !txn1_page.is_disjoint(&txn2_page),
            "page-only witnesses over-approximate and force conflicts"
        );
    }

    #[test]
    fn test_e2e_point_ops_use_cell_witnesses() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 50)], pn(4)));
        store
            .pages
            .insert(3, build_leaf_table(&[(10, b"a"), (50, b"b")]));
        store
            .pages
            .insert(4, build_leaf_table(&[(60, b"c"), (90, b"d")]));

        let cx = Cx::new();
        let mut point_cursor = BtCursor::new(store.clone(), pn(2), USABLE, true);
        let mut range_cursor = BtCursor::new(store, pn(2), USABLE, true);

        // Point workload: one hit and one miss.
        assert!(point_cursor.table_move_to(&cx, 60).unwrap().is_found());
        assert!(!point_cursor.table_move_to(&cx, 61).unwrap().is_found());
        assert!(
            point_cursor
                .witness_keys()
                .iter()
                .all(|key| matches!(key, WitnessKey::Cell { .. })),
            "point operations must not emit page-level witnesses"
        );

        // Range workload: traversal witnesses leaves at page granularity.
        assert!(range_cursor.first(&cx).unwrap());
        assert!(range_cursor.next(&cx).unwrap());
        assert!(range_cursor.next(&cx).unwrap());
        assert!(
            range_cursor
                .witness_keys()
                .iter()
                .any(|key| matches!(key, WitnessKey::Page(_))),
            "range operations may emit page-level witnesses"
        );
    }

    #[test]
    fn test_cursor_next_prev() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(
            2,
            build_leaf_table(&[(1, b"a"), (2, b"b"), (3, b"c"), (4, b"d")]),
        );

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Forward.
        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 2);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 3);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 4);
        assert!(!cursor.next(&cx).unwrap());
        assert!(cursor.eof());

        // Backward.
        assert!(cursor.last(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 4);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 3);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 2);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(!cursor.prev(&cx).unwrap());
    }

    #[test]
    fn test_cursor_empty_tree() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        assert!(!cursor.first(&cx).unwrap());
        assert!(cursor.eof());
    }

    // -- Two-level tree tests --

    #[test]
    fn test_cursor_two_level_tree_seek() {
        // Build a two-level tree:
        //   Interior page 2: children=[left=3, rowid=5], right=4
        //   Leaf page 3: (1, "a"), (5, "b")     — rowids <= 5
        //   Leaf page 4: (10, "c"), (15, "d")    — rowids > 5
        //
        // In SQLite intkey trees, the interior cell key is the max rowid
        // in the left subtree.
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 5)], pn(4)));
        store
            .pages
            .insert(3, build_leaf_table(&[(1, b"a"), (5, b"b")]));
        store
            .pages
            .insert(4, build_leaf_table(&[(10, b"c"), (15, b"d")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Seek exact matches.
        assert!(cursor.table_move_to(&cx, 1).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"a");

        assert!(cursor.table_move_to(&cx, 5).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"b");

        assert!(cursor.table_move_to(&cx, 10).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"c");

        assert!(cursor.table_move_to(&cx, 15).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"d");
    }

    #[test]
    fn test_cursor_two_level_tree_traverse() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 5)], pn(4)));
        store
            .pages
            .insert(3, build_leaf_table(&[(1, b"a"), (5, b"b")]));
        store
            .pages
            .insert(4, build_leaf_table(&[(10, b"c"), (15, b"d")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Forward traversal: 1, 5, 10, 15.
        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 5);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 10);
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 15);
        assert!(!cursor.next(&cx).unwrap());
        assert!(cursor.eof());

        // Backward traversal: 15, 10, 5, 1.
        assert!(cursor.last(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 15);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 10);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 5);
        assert!(cursor.prev(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);
        assert!(!cursor.prev(&cx).unwrap());
    }

    // -- Three-level tree test --

    #[test]
    fn test_cursor_three_level_tree() {
        // Build a three-level tree. Interior cell keys are the max rowid
        // in their left subtree.
        //
        //   Root (page 2): interior, children=[(3, 15)], right=4
        //     → left subtree (page 3) has rowids <= 15
        //     → right subtree (page 4) has rowids > 15
        //
        //   Page 3: interior, children=[(5, 3), (6, 8)], right=7
        //     → page 5 has rowids <= 3
        //     → page 6 has rowids in (3, 8]
        //     → page 7 has rowids in (8, 15]
        //
        //   Page 4: interior, children=[(8, 25)], right=9
        //     → page 8 has rowids in (15, 25]
        //     → page 9 has rowids > 25
        //
        //   Leaf pages:
        //     5: (1, 3)  6: (5, 8)  7: (10, 15)  8: (20, 25)  9: (30, 40)
        let mut store = MemPageStore::new(USABLE);

        // Root.
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 15)], pn(4)));

        // Interior pages.
        store
            .pages
            .insert(3, build_interior_table(&[(pn(5), 3), (pn(6), 8)], pn(7)));
        store
            .pages
            .insert(4, build_interior_table(&[(pn(8), 25)], pn(9)));

        // Leaves.
        store
            .pages
            .insert(5, build_leaf_table(&[(1, b"L1"), (3, b"L3")]));
        store
            .pages
            .insert(6, build_leaf_table(&[(5, b"L5"), (8, b"L8")]));
        store
            .pages
            .insert(7, build_leaf_table(&[(10, b"L10"), (15, b"L15")]));
        store
            .pages
            .insert(8, build_leaf_table(&[(20, b"L20"), (25, b"L25")]));
        store
            .pages
            .insert(9, build_leaf_table(&[(30, b"L30"), (40, b"L40")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Full forward scan.
        let mut rowids = Vec::new();
        assert!(cursor.first(&cx).unwrap());
        loop {
            rowids.push(cursor.rowid(&cx).unwrap());
            if !cursor.next(&cx).unwrap() {
                break;
            }
        }
        assert_eq!(rowids, vec![1, 3, 5, 8, 10, 15, 20, 25, 30, 40]);

        // Full backward scan.
        let mut rowids_rev = Vec::new();
        assert!(cursor.last(&cx).unwrap());
        loop {
            rowids_rev.push(cursor.rowid(&cx).unwrap());
            if !cursor.prev(&cx).unwrap() {
                break;
            }
        }
        assert_eq!(rowids_rev, vec![40, 30, 25, 20, 15, 10, 8, 5, 3, 1]);

        // Seek tests.
        assert!(cursor.table_move_to(&cx, 8).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"L8");

        assert!(cursor.table_move_to(&cx, 25).unwrap().is_found());
        assert_eq!(cursor.payload(&cx).unwrap(), b"L25");

        // Seek not found: 12 → should land on 15.
        let r = cursor.table_move_to(&cx, 12).unwrap();
        assert!(!r.is_found());
        assert_eq!(cursor.rowid(&cx).unwrap(), 15);
    }

    #[test]
    fn test_cursor_seek_then_next() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_interior_table(&[(pn(3), 5)], pn(4)));
        store
            .pages
            .insert(3, build_leaf_table(&[(1, b"one"), (5, b"five")]));
        store
            .pages
            .insert(4, build_leaf_table(&[(10, b"ten"), (20, b"twenty")]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Seek to 5, then next should give 10.
        assert!(cursor.table_move_to(&cx, 5).unwrap().is_found());
        assert!(cursor.next(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 10);
    }

    #[test]
    fn test_cursor_eof_at_payload() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[(1, b"x")]));

        let cx = Cx::new();
        let cursor = BtCursor::new(store, pn(2), USABLE, true);

        // Before first/last, cursor is at EOF.
        assert!(cursor.eof());
        assert!(cursor.payload(&cx).is_err());
        assert!(cursor.rowid(&cx).is_err());
    }

    /// Regression test for bd-14lx: sustained deletes that drain all leaf
    /// pages under one interior subtree must collapse the tree from depth 3
    /// back to depth 2 (or even depth 1).
    ///
    /// The test inserts enough data to force a depth-3 tree (root →
    /// interior → leaf), then deletes every row.  After all deletes the
    /// tree must have collapsed — either to a single leaf root page, or
    /// at least from depth 3 to a shallower structure that passes
    /// first()/next() enumeration correctly.
    #[test]
    fn test_depth3_collapse_after_sustained_deletes() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Use large payloads (~1400 bytes) so that fewer cells per leaf
        // forces deeper trees sooner.
        let mut max_rowid = 0i64;
        let mut reached_depth_3 = false;

        for rowid in 1..=2000_i64 {
            let payload = vec![b'D'; 1400];
            cursor.table_insert(&cx, rowid, &payload).unwrap();
            max_rowid = rowid;

            // Check tree depth by descending from root.
            let depth = measure_tree_depth(&cursor.pager, pn(2), USABLE);
            if depth >= 3 {
                reached_depth_3 = true;
                break;
            }
        }

        assert!(
            reached_depth_3,
            "failed to build depth-3 tree (reached rowid {max_rowid})"
        );

        // Delete every row.
        for rowid in 1..=max_rowid {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} should exist before delete");
            cursor.delete(&cx).unwrap();
        }

        // After deleting everything, the tree must be empty.
        assert!(
            !cursor.first(&cx).unwrap(),
            "tree should be empty after total delete"
        );
        assert!(cursor.eof());

        // The root page should have collapsed to a leaf (depth 1).
        let root_data = cursor.pager.read_page(&cx, pn(2)).unwrap();
        let root_header = BtreePageHeader::parse(&root_data, 0).unwrap();
        assert!(
            root_header.page_type.is_leaf(),
            "root should collapse to leaf after all rows deleted, got {:?}",
            root_header.page_type
        );
        assert_eq!(root_header.cell_count, 0);
    }

    /// Variant of the depth-3 collapse test that deletes only *some* rows,
    /// enough to drain one interior subtree, and verifies the remaining
    /// rows are still correctly enumerable.
    #[test]
    fn test_depth3_partial_delete_collapse() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        let mut max_rowid = 0i64;
        for rowid in 1..=2000_i64 {
            let payload = vec![b'P'; 1400];
            cursor.table_insert(&cx, rowid, &payload).unwrap();
            max_rowid = rowid;

            let depth = measure_tree_depth(&cursor.pager, pn(2), USABLE);
            if depth >= 3 {
                break;
            }
        }

        // Delete the first half of rows.
        let half = max_rowid / 2;
        for rowid in 1..=half {
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} should exist before delete");
            cursor.delete(&cx).unwrap();
        }

        // Verify the remaining rows are intact.
        let mut seen = 0usize;
        if cursor.first(&cx).unwrap() {
            let mut prev = i64::MIN;
            loop {
                let rowid = cursor.rowid(&cx).unwrap();
                assert!(rowid > prev, "out-of-order rowid {rowid} after {prev}");
                assert!(rowid > half, "deleted rowid {rowid} still present");
                prev = rowid;
                seen += 1;
                if !cursor.next(&cx).unwrap() {
                    break;
                }
            }
        }
        assert_eq!(
            seen,
            usize::try_from(max_rowid - half).unwrap(),
            "wrong number of surviving rows"
        );
    }

    /// Measure tree depth by descending from the root following the
    /// leftmost child at each interior level.
    fn measure_tree_depth<P: PageReader>(pager: &P, root: PageNumber, _usable: u32) -> usize {
        let cx = Cx::new();
        let mut pgno = root;
        let mut depth = 1;
        loop {
            let data = pager.read_page(&cx, pgno).unwrap();
            let offset = cell::header_offset_for_page(pgno);
            let header = cell::BtreePageHeader::parse(&data, offset).unwrap();
            if header.page_type.is_leaf() {
                return depth;
            }
            // Descend into the leftmost child.
            let ptrs = cell::read_cell_pointers(&data, &header, offset).unwrap();
            if ptrs.is_empty() {
                // Interior page with 0 cells — use right_child.
                pgno = header.right_child.unwrap();
            } else {
                // First cell's left-child pointer (first 4 bytes of cell).
                let cell_offset = ptrs[0] as usize;
                let raw = u32::from_be_bytes([
                    data[cell_offset],
                    data[cell_offset + 1],
                    data[cell_offset + 2],
                    data[cell_offset + 3],
                ]);
                let left = PageNumber::new(raw).unwrap();
                pgno = left;
            }
            depth += 1;
        }
    }

    /// Phase 3 acceptance: large overflow payloads are stored and retrieved
    /// correctly across multiple overflow pages.
    #[test]
    fn test_btree_multiple_overflow_pages() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Insert 10 rows with payloads between 5000 and 10000 bytes,
        // each requiring multiple overflow pages (page usable = 4096).
        let payloads: Vec<Vec<u8>> = (0..10)
            .map(|i| vec![b'A' + (i as u8 % 26); 5000 + i * 500])
            .collect();

        for (i, payload) in payloads.iter().enumerate() {
            let rowid = i64::try_from(i + 1).unwrap();
            cursor.table_insert(&cx, rowid, payload).unwrap();
        }

        // Verify every row round-trips exactly.
        for (i, expected) in payloads.iter().enumerate() {
            let rowid = i64::try_from(i + 1).unwrap();
            let seek = cursor.table_move_to(&cx, rowid).unwrap();
            assert!(seek.is_found(), "rowid {rowid} not found");
            let got = cursor.payload(&cx).unwrap();
            assert_eq!(
                got.len(),
                expected.len(),
                "payload length mismatch at rowid {rowid}"
            );
            assert_eq!(&got[..], &expected[..], "payload mismatch at rowid {rowid}");
        }
    }

    #[test]
    fn test_btree_overflow_page_chain_100kb() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        let payload = vec![0xCD_u8; 100 * 1024];

        cursor.table_insert(&cx, 1, &payload).unwrap();
        let seek = cursor.table_move_to(&cx, 1).unwrap();
        assert!(seek.is_found(), "expected rowid 1 to be present");

        let roundtrip = cursor.payload(&cx).unwrap();
        assert_eq!(roundtrip.len(), payload.len());
        assert_eq!(roundtrip, payload);
    }

    /// Phase 3 acceptance: page count must grow as rows are inserted
    /// (proving page splits occur), and sorted order is maintained.
    #[test]
    fn test_btree_page_count_grows_with_inserts() {
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        let cx = Cx::new();
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);

        // Insert 500 rows with ~200 byte payloads to force multiple splits.
        for i in 1..=500_i64 {
            let payload = format!("row-{i:05}-payload-data-{}", "X".repeat(180));
            cursor.table_insert(&cx, i, payload.as_bytes()).unwrap();
        }

        // The tree must have split into multiple levels.
        let depth = measure_tree_depth(&cursor.pager, pn(2), USABLE);
        assert!(
            depth > 1,
            "expected tree depth > 1 after 500 inserts, got {depth}"
        );

        // Full forward scan must yield 500 rows in sorted order.
        assert!(cursor.first(&cx).unwrap());
        let mut count = 1u32;
        let mut prev = cursor.rowid(&cx).unwrap();
        while cursor.next(&cx).unwrap() {
            let current = cursor.rowid(&cx).unwrap();
            assert!(current > prev, "sort violation: {current} followed {prev}");
            prev = current;
            count += 1;
        }
        assert_eq!(count, 500, "expected 500 rows, saw {count}");
    }

    proptest::proptest! {
        /// Property: after arbitrary insert/delete sequences the B-tree
        /// always maintains sorted rowid order when scanned.
        #[test]
        fn prop_btree_order_invariant(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    (1..=5000_i64, proptest::collection::vec(proptest::num::u8::ANY, 10..200))
                        .prop_map(|(r, p)| (true, r, p)),
                    (1..=5000_i64,).prop_map(|(r,)| (false, r, Vec::new())),
                ],
                1..200
            )
        ) {
            let mut store = MemPageStore::new(USABLE);
            store.pages.insert(2, build_leaf_table(&[]));

            let cx = Cx::new();
            let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
            let mut live: BTreeSet<i64> = BTreeSet::new();

            for (is_insert, rowid, payload) in &ops {
                if *is_insert && !live.contains(rowid) {
                    cursor.table_insert(&cx, *rowid, payload).unwrap();
                    live.insert(*rowid);
                } else if !*is_insert && live.contains(rowid) {
                    let seek = cursor.table_move_to(&cx, *rowid).unwrap();
                    if seek.is_found() {
                        cursor.delete(&cx).unwrap();
                        live.remove(rowid);
                    }
                }
            }

            // Verify sorted order and correct count.
            let mut scanned = Vec::new();
            if cursor.first(&cx).unwrap() {
                loop {
                    scanned.push(cursor.rowid(&cx).unwrap());
                    if !cursor.next(&cx).unwrap() {
                        break;
                    }
                }
            }

            // Rowids must be strictly ascending.
            for window in scanned.windows(2) {
                proptest::prop_assert!(
                    window[0] < window[1],
                    "sort violation: {} >= {}",
                    window[0],
                    window[1]
                );
            }
            proptest::prop_assert_eq!(scanned.len(), live.len());
        }

        /// bd-2sm1: B-tree order matches BTreeMap reference after random ops.
        /// Unlike prop_btree_order_invariant, this allows duplicate inserts
        /// (which produce PrimaryKeyViolation) and verifies exact rowid-set
        /// equality with a reference BTreeMap.
        #[test]
        fn prop_btree_vs_btreemap_reference(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    3 => (1..=2000_i64, proptest::collection::vec(proptest::num::u8::ANY, 10..100))
                        .prop_map(|(r, p)| (true, r, p)),
                    1 => (1..=2000_i64,).prop_map(|(r,)| (false, r, Vec::new())),
                ],
                1..500
            )
        ) {
            let mut store = MemPageStore::new(USABLE);
            store.pages.insert(2, build_leaf_table(&[]));

            let cx = Cx::new();
            let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
            let mut reference: std::collections::BTreeMap<i64, Vec<u8>> =
                std::collections::BTreeMap::new();

            for (is_insert, rowid, payload) in &ops {
                if *is_insert {
                    if reference.contains_key(rowid) {
                        // Duplicate: inserting an existing rowid must fail.
                        let result = cursor.table_insert(&cx, *rowid, payload);
                        proptest::prop_assert!(
                            matches!(result, Err(FrankenError::PrimaryKeyViolation)),
                            "duplicate rowid {} should produce PrimaryKeyViolation, got {:?}",
                            rowid,
                            result,
                        );
                    } else {
                        cursor.table_insert(&cx, *rowid, payload).unwrap();
                        reference.insert(*rowid, payload.clone());
                    }
                } else if reference.contains_key(rowid) {
                    let seek = cursor.table_move_to(&cx, *rowid).unwrap();
                    if seek.is_found() {
                        cursor.delete(&cx).unwrap();
                        reference.remove(rowid);
                    }
                }
            }

            // Scan and compare with reference.
            let mut scanned_rowids = Vec::new();
            if cursor.first(&cx).unwrap() {
                loop {
                    scanned_rowids.push(cursor.rowid(&cx).unwrap());
                    if !cursor.next(&cx).unwrap() {
                        break;
                    }
                }
            }

            let ref_rowids: Vec<i64> = reference.keys().copied().collect();
            proptest::prop_assert_eq!(
                &scanned_rowids,
                &ref_rowids,
                "bead_id=bd-2sm1 case=btree_vs_btreemap rowids mismatch"
            );
        }
    }

    #[test]
    fn test_real_cursor_revives_from_eof() {
        let cx = Cx::new();
        let mut store = MemPageStore::new(USABLE);
        store.pages.insert(2, build_leaf_table(&[]));

        // Insert a few records into a leaf
        let mut cursor = BtCursor::new(PrefetchProbeStore::new(store), pn(2), USABLE, true);
        cursor.table_insert(&cx, 1, b"one").unwrap();
        cursor.table_insert(&cx, 2, b"two").unwrap();

        cursor.first(&cx).unwrap();
        cursor.next(&cx).unwrap(); // at 2
        assert!(!cursor.next(&cx).unwrap()); // now at EOF

        assert!(cursor.eof());

        // REVIVE FROM EOF
        let revived = cursor.prev(&cx).unwrap();
        assert!(revived, "Real cursor should revive from EOF");
        assert!(!cursor.eof());
        assert_eq!(cursor.rowid(&cx).unwrap(), 2);
    }

    #[test]
    fn test_table_move_to_honors_cancelled_context() {
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_leaf_table(&[(1, b"one"), (2, b"two")]));

        let mut cursor = BtCursor::new(store, pn(2), USABLE, true);
        let cx = Cx::new();
        cx.transition_to_running();
        cx.cancel_with_reason(fsqlite_types::cx::CancelReason::UserInterrupt);

        let err = cursor.table_move_to(&cx, 2).unwrap_err();
        assert!(matches!(err, FrankenError::Abort));
        assert!(
            cursor.stack.is_empty(),
            "cancelled seek should not mutate stack"
        );
    }

    #[test]
    fn test_next_honors_cancelled_context() {
        let cx = Cx::new();
        let mut store = MemPageStore::new(USABLE);
        store
            .pages
            .insert(2, build_leaf_table(&[(1, b"one"), (2, b"two")]));

        let mut cursor = BtCursor::new(store, pn(2), USABLE, true);
        assert!(cursor.first(&cx).unwrap());
        assert_eq!(cursor.rowid(&cx).unwrap(), 1);

        cx.transition_to_running();
        cx.cancel_with_reason(fsqlite_types::cx::CancelReason::UserInterrupt);

        let err = cursor.next(&cx).unwrap_err();
        assert!(matches!(err, FrankenError::Abort));
        assert_eq!(
            cursor.rowid(&Cx::new()).unwrap(),
            1,
            "cancelled iteration should preserve the prior cursor position"
        );
    }

    /// bd-wwqen.1: count_all_rows must return the correct count for empty,
    /// root-only (single-leaf), and multi-leaf (interior-node) trees.
    #[test]
    fn test_count_all_rows_empty_root_only_and_multi_leaf() {
        const USABLE: u32 = 4096;
        let cx = Cx::new();
        let root = PageNumber::new(2).unwrap();

        // ── Empty tree: zero rows ──
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);
        assert_eq!(
            cursor.count_all_rows(&cx).unwrap(),
            0,
            "bd-wwqen.1: empty table must return count 0"
        );

        // ── Root-only tree: small number of rows in a single leaf ──
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);
        for i in 1..=5_i64 {
            cursor
                .table_insert(&cx, i, format!("row-{i}").as_bytes())
                .expect("insert should succeed");
        }
        assert_eq!(
            cursor.count_all_rows(&cx).unwrap(),
            5,
            "bd-wwqen.1: root-only table with 5 rows must return count 5"
        );

        // ── Multi-leaf tree: enough rows to force page splits ──
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);
        let n = 500;
        let payload = vec![b'X'; 200]; // ~200 bytes per row → ~20 rows/page → ~25 pages
        for i in 1..=n {
            cursor
                .table_insert(&cx, i, &payload)
                .expect("insert should succeed");
        }
        let count = cursor.count_all_rows(&cx).unwrap();
        assert_eq!(
            count, n,
            "bd-wwqen.1: multi-leaf table with {n} rows must return count {n}, got {count}"
        );

        // ── count_all_rows preserves cursor usability ──
        // After count, cursor should still be usable for a seek.
        assert!(
            cursor
                .table_move_to(&cx, 1)
                .expect("seek after count should succeed")
                .is_found(),
            "bd-wwqen.1: cursor must remain usable after count_all_rows"
        );
    }

    #[test]
    fn test_count_all_rows_deep_tree_rightmost_10k() {
        const USABLE: u32 = 4096;
        let cx = Cx::new();
        let root = PageNumber::new(2).unwrap();
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);
        let row_count = 10_000_i64;
        let payload = vec![b'R'; 200];

        for rowid in 1..=row_count {
            cursor
                .table_insert(&cx, rowid, &payload)
                .expect("insert should succeed");
        }

        let depth = measure_tree_depth(&cursor.pager, root, USABLE);
        assert!(
            depth >= 3,
            "test requires a deeper interior tree, got depth {depth}"
        );

        let count = cursor.count_all_rows(&cx).unwrap();
        assert_eq!(
            count, row_count,
            "deep/rightmost table with {row_count} rows must count exactly"
        );

        assert!(
            cursor
                .table_move_to(&cx, row_count)
                .expect("seek after deep count should succeed")
                .is_found(),
            "rightmost row must remain reachable after count_all_rows"
        );
    }

    #[test]
    fn test_find_child_slot_by_page_no_matches_actual_root_children() {
        const USABLE: u32 = 4096;
        let cx = Cx::new();
        let root = PageNumber::new(2).unwrap();
        let store = MemPageStore::with_empty_table(root, USABLE);
        let mut cursor = BtCursor::new(store, root, USABLE, true);
        let payload = vec![b'S'; 200];

        for rowid in 1..=10_000_i64 {
            cursor
                .table_insert(&cx, rowid, &payload)
                .expect("insert should succeed");
        }

        let root_entry = cursor
            .reload_page_fresh(&cx, root)
            .expect("reload root after inserts");
        assert!(
            root_entry.header.page_type.is_interior(),
            "expected interior root after enough inserts"
        );

        for child_idx in 0..=root_entry.header.cell_count {
            let child_page = cursor
                .child_page_at(&root_entry, child_idx)
                .expect("read child pointer");
            let found = cursor
                .find_child_slot_by_page_no(&cx, root, child_page)
                .expect("find child slot");
            assert_eq!(
                found, child_idx,
                "slot lookup must round-trip for child {} on root page {}",
                child_page, root
            );
        }
    }
}
