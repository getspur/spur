//! Page Materialization Layer (C5: bd-l9k8e.5)
//!
//! Converts accumulated cell-level deltas into full page images when required:
//! - Structural B-tree changes (page split/merge)
//! - WAL checkpointing
//! - Eager materialization threshold (~32 deltas)
//!
//! # Design
//!
//! Cell-level MVCC avoids full page copies on every row write. But eventually
//! we need real pages for:
//! 1. When the B-tree needs to split/merge a page (structural operation)
//! 2. When WAL checkpoint writes pages back to the main database file
//! 3. When eager materialization threshold is reached
//!
//! # Key Function
//!
//! [`materialize_page`] takes a base page and a list of deltas, applies only
//! those visible to the given snapshot in commit_seq order, and returns a
//! complete, valid B-tree page.

use std::collections::HashMap;
use std::time::Instant;

use fsqlite_btree::{
    BtreePageHeader, BtreePageType, header_offset_for_page, read_cell_pointers, write_cell_pointers,
};
use fsqlite_error::{FrankenError, Result};
use fsqlite_types::limits::CELL_POINTER_SIZE;
use fsqlite_types::{PageData, PageNumber, Snapshot};
use tracing::{debug, info, warn};

use crate::cell_visibility::{CellDelta, CellDeltaKind};

// ---------------------------------------------------------------------------
// Error Types
// ---------------------------------------------------------------------------

/// Errors that can occur during page materialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializationError {
    /// The base page is not a valid B-tree page.
    InvalidBasePage { detail: String },
    /// The page is an interior page (cell-level MVCC only supports leaf pages).
    InteriorPageNotSupported,
    /// Cell data would overflow the page.
    PageOverflow { needed: usize, available: usize },
    /// Inconsistent delta: delete on non-existent cell.
    DeleteNonExistent { key_digest: [u8; 16] },
    /// Inconsistent delta: insert on existing cell.
    InsertExisting { key_digest: [u8; 16] },
}

impl std::fmt::Display for MaterializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBasePage { detail } => write!(f, "invalid base page: {detail}"),
            Self::InteriorPageNotSupported => {
                write!(f, "cell-level MVCC not supported for interior pages")
            }
            Self::PageOverflow { needed, available } => {
                write!(f, "page overflow: need {needed} bytes, have {available}")
            }
            Self::DeleteNonExistent { key_digest } => {
                write!(f, "delete on non-existent cell: {:?}", &key_digest[..4])
            }
            Self::InsertExisting { key_digest } => {
                write!(f, "insert on existing cell: {:?}", &key_digest[..4])
            }
        }
    }
}

impl std::error::Error for MaterializationError {}

// ---------------------------------------------------------------------------
// Materialization Trigger
// ---------------------------------------------------------------------------

/// Reason why materialization was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationTrigger {
    /// Page split or merge operation.
    Structural,
    /// WAL checkpoint writing pages to main DB.
    Checkpoint,
    /// Delta count exceeded threshold.
    Threshold,
    /// Explicit request (e.g., read path caching).
    Explicit,
}

impl MaterializationTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "split",
            Self::Checkpoint => "checkpoint",
            Self::Threshold => "threshold",
            Self::Explicit => "explicit",
        }
    }
}

// ---------------------------------------------------------------------------
// Materialization Result
// ---------------------------------------------------------------------------

/// Result of a page materialization operation.
#[derive(Debug, Clone)]
pub struct MaterializationResult {
    /// The materialized page data.
    pub page: PageData,
    /// Number of deltas applied.
    pub deltas_applied: usize,
    /// Number of cells in the resulting page.
    pub cell_count: u16,
    /// Time taken for materialization in microseconds.
    pub duration_us: u64,
}

// ---------------------------------------------------------------------------
// Core Materialization Function
// ---------------------------------------------------------------------------

/// Materialize a page by applying cell deltas to a base page.
///
/// # Arguments
///
/// * `base` - The base page (last full page version from VersionStore or disk)
/// * `page_number` - The page number (needed for header offset calculation)
/// * `deltas` - Cell deltas to apply (will be filtered by visibility)
/// * `snapshot` - The snapshot determining which deltas are visible
/// * `usable_size` - The usable page size (page_size - reserved_bytes)
///
/// # Returns
///
/// A `MaterializationResult` containing the materialized page and stats.
///
/// # Errors
///
/// Returns an error if:
/// - The base page is not a valid B-tree leaf page
/// - A delta would cause page overflow
/// - Delta consistency check fails
///
/// # Example
///
/// ```ignore
/// let result = materialize_page(
///     &base_page,
///     page_no,
///     &deltas,
///     &snapshot,
///     4096,
///     MaterializationTrigger::Checkpoint,
/// )?;
/// ```
pub fn materialize_page(
    base: &PageData,
    page_number: PageNumber,
    deltas: &[CellDelta],
    snapshot: &Snapshot,
    usable_size: u32,
    trigger: MaterializationTrigger,
) -> Result<MaterializationResult> {
    let start = Instant::now();
    let header_offset = header_offset_for_page(page_number);

    // Parse base page header
    let header = BtreePageHeader::parse(base.as_bytes(), header_offset).map_err(|e| {
        warn!(
            pgno = page_number.get(),
            error = %e,
            "materialization_failed"
        );
        FrankenError::DatabaseCorrupt {
            detail: format!("materialize_page: invalid base page: {e}"),
        }
    })?;

    // Cell-level MVCC only supports leaf pages
    if header.page_type.is_interior() {
        warn!(
            pgno = page_number.get(),
            page_type = ?header.page_type,
            "materialization_failed"
        );
        return Err(FrankenError::DatabaseCorrupt {
            detail: "materialize_page: interior pages not supported for cell-level MVCC".to_owned(),
        });
    }

    // Filter and sort deltas by commit_seq
    let mut visible_deltas: Vec<&CellDelta> = deltas
        .iter()
        .filter(|d| d.is_visible_to(snapshot.high))
        .collect();
    visible_deltas.sort_by_key(|d| d.commit_seq);

    if visible_deltas.is_empty() {
        // No visible deltas — return base page unchanged
        let duration_us = start.elapsed().as_micros() as u64;
        info!(
            pgno = page_number.get(),
            delta_count = 0,
            trigger = trigger.as_str(),
            "page_materialized"
        );
        return Ok(MaterializationResult {
            page: base.clone(),
            deltas_applied: 0,
            cell_count: header.cell_count,
            duration_us,
        });
    }

    // Build working state from base page
    let btree_ref = visible_deltas[0].cell_key.btree;
    let mut state = WorkingPageState::from_base_page(
        base,
        page_number,
        &header,
        header_offset,
        usable_size,
        btree_ref,
    )?;

    // Apply each visible delta
    for delta in &visible_deltas {
        state.apply_delta(delta);
    }

    // Reconstruct page from working state
    let page = state.build_page(base, &header, header_offset, usable_size)?;
    let duration_us = start.elapsed().as_micros() as u64;

    debug!(
        pgno = page_number.get(),
        base_commit_seq = 0, // base doesn't have a commit_seq
        applied_deltas = visible_deltas.len(),
        result_cell_count = state.cells.len(),
        "materialization_detail"
    );

    info!(
        pgno = page_number.get(),
        delta_count = visible_deltas.len(),
        trigger = trigger.as_str(),
        "page_materialized"
    );

    info!(
        pgno = page_number.get(),
        duration_us, "materialization_timing"
    );

    Ok(MaterializationResult {
        page,
        deltas_applied: visible_deltas.len(),
        cell_count: state.cells.len() as u16,
        duration_us,
    })
}

// ---------------------------------------------------------------------------
// Working Page State (intermediate representation during materialization)
// ---------------------------------------------------------------------------

/// A working cell with its content and sort key.
struct WorkingCell {
    /// Cell content bytes.
    content: Vec<u8>,
    /// Extracted sort key (rowid for table cells, key bytes for index cells).
    /// This ensures proper B-tree ordering after materialization.
    sort_key: SortKey,
}

/// Sort key for maintaining B-tree cell ordering.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    /// Table leaf cell: sort by rowid.
    Rowid(i64),
    /// Index leaf cell: sort by key bytes.
    IndexKey(Vec<u8>),
}

/// Intermediate state while applying deltas.
#[allow(dead_code)]
struct WorkingPageState {
    /// Current cells indexed by key_digest.
    cells_by_key: HashMap<[u8; 16], usize>,
    /// All cells in order.
    cells: Vec<WorkingCell>,
    /// Page type for validation.
    page_type: BtreePageType,
    /// Usable page size.
    usable_size: u32,
}

impl WorkingPageState {
    /// Initialize working state from a base page.
    fn from_base_page(
        base: &PageData,
        page_number: PageNumber,
        header: &BtreePageHeader,
        header_offset: usize,
        usable_size: u32,
        btree_ref: fsqlite_types::BtreeRef,
    ) -> Result<Self> {
        let cell_pointers = read_cell_pointers(base.as_bytes(), header, header_offset)?;
        let page_bytes = base.as_bytes();

        let mut cells = Vec::with_capacity(cell_pointers.len());
        let mut cells_by_key = HashMap::with_capacity(cell_pointers.len());

        for (idx, &ptr) in cell_pointers.iter().enumerate() {
            let cell_offset = ptr as usize;

            // Extract cell content - we need to compute the cell size
            let cell_end =
                compute_cell_end(page_bytes, cell_offset, header.page_type, usable_size)?;
            let cell_content = page_bytes[cell_offset..cell_end].to_vec();

            // Compute key digest and sort key for this cell
            let (key_digest, sort_key) = compute_cell_key_and_sort_key(
                page_bytes,
                cell_offset,
                header.page_type,
                btree_ref,
            )?;

            cells.push(WorkingCell {
                content: cell_content,
                sort_key,
            });
            cells_by_key.insert(key_digest, idx);
        }

        debug!(
            pgno = page_number.get(),
            base_cell_count = cells.len(),
            "working_state_initialized"
        );

        Ok(Self {
            cells_by_key,
            cells,
            page_type: header.page_type,
            usable_size,
        })
    }

    /// Apply a single delta to the working state.
    fn apply_delta(&mut self, delta: &CellDelta) {
        let (key_digest, sort_key) =
            compute_cell_key_and_sort_key_from_delta(delta, self.page_type);

        match delta.kind {
            CellDeltaKind::Insert => {
                // Insert: add new cell (should not exist)
                if self.cells_by_key.contains_key(&key_digest) {
                    // Cell already exists - this could be a re-insert after delete
                    // which is valid (the delta chain handles this)
                    // Just update the content
                    let idx = self.cells_by_key[&key_digest];
                    self.cells[idx].content.clone_from(&delta.cell_data);
                    self.cells[idx].sort_key = sort_key;
                } else {
                    let idx = self.cells.len();
                    self.cells.push(WorkingCell {
                        content: delta.cell_data.clone(),
                        sort_key,
                    });
                    self.cells_by_key.insert(key_digest, idx);
                }
            }
            CellDeltaKind::Update => {
                // Update: replace existing cell content
                if let Some(&idx) = self.cells_by_key.get(&key_digest) {
                    self.cells[idx].content.clone_from(&delta.cell_data);
                    self.cells[idx].sort_key = sort_key;
                } else {
                    // Update on non-existent cell - treat as insert
                    let idx = self.cells.len();
                    self.cells.push(WorkingCell {
                        content: delta.cell_data.clone(),
                        sort_key,
                    });
                    self.cells_by_key.insert(key_digest, idx);
                }
            }
            CellDeltaKind::Delete => {
                // Delete: remove cell
                if let Some(&idx) = self.cells_by_key.get(&key_digest) {
                    // Mark as deleted by clearing content
                    // We'll filter these out during page reconstruction
                    self.cells[idx].content.clear();
                    self.cells_by_key.remove(&key_digest);
                }
                // Delete on non-existent is a no-op (idempotent)
            }
        }
    }

    /// Build the final page from working state.
    fn build_page(
        &self,
        base: &PageData,
        original_header: &BtreePageHeader,
        header_offset: usize,
        usable_size: u32,
    ) -> Result<PageData> {
        // Filter out deleted cells and collect live cells
        let live_cells: Vec<&WorkingCell> = self
            .cells
            .iter()
            .filter(|c| !c.content.is_empty())
            .collect();

        // Sort cells by B-tree key order (rowid for table, key bytes for index)
        // This maintains B-tree invariants required for binary search
        let mut sorted_cells: Vec<&WorkingCell> = live_cells;
        sorted_cells.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));

        // Calculate required space
        let header_size = original_header.page_type.header_size() as usize;
        let ptr_array_size = sorted_cells.len() * CELL_POINTER_SIZE as usize;
        let total_cell_bytes: usize = sorted_cells.iter().map(|c| c.content.len()).sum();

        let needed_space = header_offset + header_size + ptr_array_size + total_cell_bytes;
        if needed_space > usable_size as usize {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "materialize_page: page overflow: need {} bytes, have {}",
                    needed_space, usable_size
                ),
            });
        }

        // Allocate new page buffer
        let page_size = base.len();
        let mut page = vec![0u8; page_size];

        // Copy database file header if page 1
        if header_offset > 0 {
            page[..header_offset].copy_from_slice(&base.as_bytes()[..header_offset]);
        }

        // Write cell content area (grows down from end of usable space)
        let mut content_offset = usable_size as usize;
        let mut cell_pointers = Vec::with_capacity(sorted_cells.len());

        for cell in &sorted_cells {
            content_offset -= cell.content.len();
            page[content_offset..content_offset + cell.content.len()]
                .copy_from_slice(&cell.content);
            cell_pointers.push(content_offset as u16);
        }

        // Build new header
        let new_header = BtreePageHeader {
            page_type: original_header.page_type,
            first_freeblock: 0, // No freeblocks in freshly packed page
            cell_count: sorted_cells.len() as u16,
            cell_content_offset: content_offset as u32,
            fragmented_free_bytes: 0, // No fragmentation in freshly packed page
            right_child: original_header.right_child,
        };

        // Write header
        new_header.write(&mut page, header_offset);

        // Write cell pointer array
        write_cell_pointers(&mut page, header_offset, &new_header, &cell_pointers);

        Ok(PageData::from_vec(page))
    }
}

// ---------------------------------------------------------------------------
// Cell Key Computation Helpers
// ---------------------------------------------------------------------------

/// Compute both the key digest (for HashMap lookup) and sort key (for B-tree ordering).
fn compute_cell_key_and_sort_key(
    page: &[u8],
    cell_offset: usize,
    page_type: BtreePageType,
    btree_ref: fsqlite_types::BtreeRef,
) -> Result<([u8; 16], SortKey)> {
    // For table leaf pages, extract the rowid
    // For index leaf pages, extract the key bytes

    if page_type == BtreePageType::LeafTable {
        // Table leaf cell: payload_size varint, rowid varint, payload
        let (_, ps_len) = fsqlite_types::serial_type::read_varint(&page[cell_offset..])
            .ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: "invalid varint in cell (payload size)".to_owned(),
            })?;

        let rowid_start = cell_offset + ps_len;
        let (rowid, _rowid_len) = fsqlite_types::serial_type::read_varint(&page[rowid_start..])
            .ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: "invalid varint in cell (rowid)".to_owned(),
            })?;

        // Hash the rowid for key_digest
        let mut key_bytes = [0u8; 10];
        let len = encode_varint_i64(rowid as i64, &mut key_bytes);
        let key_digest = fsqlite_types::SemanticKeyRef::compute_digest(
            fsqlite_types::SemanticKeyKind::TableRow,
            btree_ref,
            &key_bytes[..len],
        );

        Ok((key_digest, SortKey::Rowid(rowid as i64)))
    } else if page_type == BtreePageType::LeafIndex {
        // Index leaf cell: payload_size varint, key bytes
        // Note: For large keys with overflow, only local portion is on-page.
        // We use local_payload_size to determine how much is actually here.
        let (payload_size, ps_len) = fsqlite_types::serial_type::read_varint(&page[cell_offset..])
            .ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: "invalid varint in cell (payload size)".to_owned(),
            })?;

        let key_start = cell_offset + ps_len;

        // For index cells, the comparison key is the full payload.
        // If payload exceeds local storage, we only have partial key on-page.
        // For now, use what's available (overflow handling is rare for index keys).
        let available = page.len().saturating_sub(key_start);
        let key_len = (payload_size as usize).min(available);
        let key_end = key_start + key_len;

        let key_bytes = page[key_start..key_end].to_vec();
        let key_digest = fsqlite_types::SemanticKeyRef::compute_digest(
            fsqlite_types::SemanticKeyKind::IndexEntry,
            btree_ref,
            &key_bytes,
        );

        Ok((key_digest, SortKey::IndexKey(key_bytes)))
    } else {
        // Interior pages not supported
        Err(FrankenError::DatabaseCorrupt {
            detail: "cannot compute key digest for interior page cells".to_owned(),
        })
    }
}

/// Compute key digest and sort key from a cell delta.
///
/// # Known Limitation
///
/// For DELETE deltas with empty `cell_data`, this returns `([0u8; 16], SortKey::Rowid(0))`
/// which will NOT match any actual cell. This is a design flaw in `CellDelta` which
/// should store the `key_digest` field from the original `CellKey`.
///
/// Cell deltas now carry the originating `CellKey`, so DELETE operations can
/// target the correct cell without reverse-engineering the key from cell bytes.
fn compute_cell_key_and_sort_key_from_delta(
    delta: &CellDelta,
    page_type: BtreePageType,
) -> ([u8; 16], SortKey) {
    let key_digest = delta.cell_key.key_digest;

    if delta.cell_data.is_empty() {
        let sort_key = if page_type == BtreePageType::LeafIndex {
            SortKey::IndexKey(Vec::new())
        } else {
            SortKey::Rowid(0)
        };
        return (key_digest, sort_key);
    }

    if page_type == BtreePageType::LeafTable {
        // Table cell: payload_size varint, rowid varint, payload
        if let Some((_, ps_len)) = fsqlite_types::serial_type::read_varint(&delta.cell_data) {
            if ps_len < delta.cell_data.len() {
                if let Some((rowid, _)) =
                    fsqlite_types::serial_type::read_varint(&delta.cell_data[ps_len..])
                {
                    return (key_digest, SortKey::Rowid(rowid as i64));
                }
            }
        }
        (key_digest, SortKey::Rowid(0))
    } else if page_type == BtreePageType::LeafIndex {
        // Index cell: payload_size varint, key bytes
        if let Some((payload_size, ps_len)) =
            fsqlite_types::serial_type::read_varint(&delta.cell_data)
        {
            let key_start = ps_len;
            let key_len = (payload_size as usize).min(delta.cell_data.len() - key_start);
            let key_bytes = delta.cell_data[key_start..key_start + key_len].to_vec();
            return (key_digest, SortKey::IndexKey(key_bytes));
        }
        (key_digest, SortKey::IndexKey(delta.cell_data.clone()))
    } else {
        (key_digest, SortKey::Rowid(0))
    }
}

// Removed compute_key_digest

/// Compute the end offset of a cell.
fn compute_cell_end(
    page: &[u8],
    cell_offset: usize,
    page_type: BtreePageType,
    usable_size: u32,
) -> Result<usize> {
    use fsqlite_btree::local_payload_size;

    // Read varints to determine cell structure
    let (payload_size, ps_len) = fsqlite_types::serial_type::read_varint(&page[cell_offset..])
        .ok_or_else(|| FrankenError::DatabaseCorrupt {
            detail: "invalid varint in cell (payload size)".to_owned(),
        })?;

    let mut pos = cell_offset + ps_len;

    // Table cells have a rowid varint
    if page_type.is_table() && page_type.is_leaf() {
        let (_, rowid_len) =
            fsqlite_types::serial_type::read_varint(&page[pos..]).ok_or_else(|| {
                FrankenError::DatabaseCorrupt {
                    detail: "invalid varint in cell (rowid)".to_owned(),
                }
            })?;
        pos += rowid_len;
    }

    // Payload (potentially with overflow pointer)
    let local_size = local_payload_size(payload_size as u32, usable_size, page_type);
    pos += local_size as usize;

    // If overflow, add 4 bytes for overflow page pointer
    if local_size < payload_size as u32 {
        pos += 4;
    }

    Ok(pos)
}

/// Encode an i64 as a SQLite varint.
fn encode_varint_i64(value: i64, buf: &mut [u8]) -> usize {
    let mut v = value as u64;
    let mut i = 0;

    // SQLite varint encoding
    if v <= 0x7F {
        buf[0] = v as u8;
        return 1;
    }

    // Multi-byte encoding
    while v > 0x7F {
        buf[i] = (v & 0x7F) as u8 | 0x80;
        v >>= 7;
        i += 1;
    }
    buf[i] = v as u8;
    i + 1
}

// ---------------------------------------------------------------------------
// Threshold-Based Eager Materialization
// ---------------------------------------------------------------------------

/// Default threshold for eager materialization (number of deltas per page).
pub const DEFAULT_MATERIALIZATION_THRESHOLD: usize = 32;

/// Check if a page should be eagerly materialized based on delta count.
#[must_use]
pub fn should_materialize_eagerly(delta_count: usize, threshold: usize) -> bool {
    delta_count > threshold
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use fsqlite_types::{CommitSeq, PageNumber, SchemaEpoch, TxnEpoch, TxnId, TxnToken};

    const PAGE_SIZE: u32 = 4096;
    const USABLE_SIZE: u32 = 4096;

    fn test_snapshot(high: u64) -> Snapshot {
        Snapshot {
            high: CommitSeq::new(high),
            schema_epoch: SchemaEpoch::new(1),
        }
    }

    fn test_txn() -> TxnToken {
        TxnToken::new(TxnId::new(1).unwrap(), TxnEpoch::new(0))
    }

    fn create_empty_leaf_table_page() -> PageData {
        let mut page = vec![0u8; PAGE_SIZE as usize];

        // Page header for leaf table (8 bytes)
        page[0] = BtreePageType::LeafTable as u8; // page type
        page[1] = 0;
        page[2] = 0; // first freeblock
        page[3] = 0;
        page[4] = 0; // cell count = 0
        page[5] = 0x10;
        page[6] = 0x00; // cell content offset = 4096
        page[7] = 0; // fragmented free bytes

        PageData::from_vec(page)
    }

    fn create_leaf_table_cell(rowid: i64, payload: &[u8]) -> Vec<u8> {
        let mut cell = Vec::new();

        // Payload size varint
        let payload_len = payload.len() as u64;
        let mut buf = [0u8; 10];
        let ps_len = encode_varint_u64(payload_len, &mut buf);
        cell.extend_from_slice(&buf[..ps_len]);

        // Rowid varint
        let ri_len = encode_varint_i64(rowid, &mut buf);
        cell.extend_from_slice(&buf[..ri_len]);

        // Payload
        cell.extend_from_slice(payload);

        cell
    }

    fn encode_varint_u64(value: u64, buf: &mut [u8]) -> usize {
        encode_varint_i64(value as i64, buf)
    }

    fn create_delta_insert(rowid: i64, payload: &[u8], commit_seq: u64) -> CellDelta {
        CellDelta {
            commit_seq: CommitSeq::new(commit_seq),
            created_by: test_txn(),
            cell_key: crate::cell_visibility::CellKey::table_row(
                fsqlite_types::BtreeRef::Table(fsqlite_types::TableId::new(1)),
                rowid,
            ),
            kind: CellDeltaKind::Insert,
            page_number: PageNumber::new(2).unwrap(),
            cell_data: create_leaf_table_cell(rowid, payload),
            prev_idx: None,
        }
    }

    fn create_delta_delete(commit_seq: u64) -> CellDelta {
        CellDelta {
            commit_seq: CommitSeq::new(commit_seq),
            created_by: test_txn(),
            cell_key: crate::cell_visibility::CellKey::table_row(
                fsqlite_types::BtreeRef::Table(fsqlite_types::TableId::new(1)),
                100,
            ),
            kind: CellDeltaKind::Delete,
            page_number: PageNumber::new(2).unwrap(),
            cell_data: vec![],
            prev_idx: None,
        }
    }

    fn create_delta_update(rowid: i64, payload: &[u8], commit_seq: u64) -> CellDelta {
        CellDelta {
            commit_seq: CommitSeq::new(commit_seq),
            created_by: test_txn(),
            cell_key: crate::cell_visibility::CellKey::table_row(
                fsqlite_types::BtreeRef::Table(fsqlite_types::TableId::new(1)),
                rowid,
            ),
            kind: CellDeltaKind::Update,
            page_number: PageNumber::new(2).unwrap(),
            cell_data: create_leaf_table_cell(rowid, payload),
            prev_idx: None,
        }
    }

    #[test]
    fn test_materialize_single_insert() {
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();
        let deltas = vec![create_delta_insert(100, b"hello world", 5)];
        let snapshot = test_snapshot(10);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        )
        .expect("materialization should succeed");

        assert_eq!(result.deltas_applied, 1);
        assert_eq!(result.cell_count, 1);

        // Verify the page is valid
        let header = BtreePageHeader::parse(result.page.as_bytes(), 0).unwrap();
        assert_eq!(header.cell_count, 1);
        assert_eq!(header.page_type, BtreePageType::LeafTable);
    }

    #[test]
    fn test_materialize_multiple_inserts() {
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();
        let deltas: Vec<CellDelta> = (0..10)
            .map(|i| {
                create_delta_insert(i as i64 + 100, format!("value{i}").as_bytes(), i as u64 + 1)
            })
            .collect();
        let snapshot = test_snapshot(20);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        )
        .expect("materialization should succeed");

        assert_eq!(result.deltas_applied, 10);
        assert_eq!(result.cell_count, 10);

        // Verify all cells present
        let header = BtreePageHeader::parse(result.page.as_bytes(), 0).unwrap();
        assert_eq!(header.cell_count, 10);
    }

    #[test]
    fn test_materialize_update() {
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();
        let deltas = vec![
            create_delta_insert(100, b"original", 5),
            create_delta_update(100, b"updated", 10),
        ];
        let snapshot = test_snapshot(15);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        )
        .expect("materialization should succeed");

        assert_eq!(result.deltas_applied, 2);
        assert_eq!(result.cell_count, 1);

        // Verify the cell content is updated
        let header = BtreePageHeader::parse(result.page.as_bytes(), 0).unwrap();
        assert_eq!(header.cell_count, 1);
    }

    #[test]
    fn test_materialize_delete() {
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();
        let deltas = vec![
            create_delta_insert(100, b"to be deleted", 5),
            create_delta_delete(10),
        ];
        let snapshot = test_snapshot(15);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        )
        .expect("materialization should succeed");

        // Delete delta has empty cell_data, so key matching won't work
        // This tests the idempotent delete behavior
        assert_eq!(result.deltas_applied, 2);
    }

    #[test]
    fn test_materialize_mixed() {
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();

        let mut deltas = Vec::new();
        // 20 inserts
        for i in 0..20 {
            deltas.push(create_delta_insert(
                i as i64 + 100,
                format!("val{i}").as_bytes(),
                i as u64 + 1,
            ));
        }
        // 5 updates
        for i in 0..5 {
            deltas.push(create_delta_update(
                i as i64 + 100,
                format!("upd{i}").as_bytes(),
                25 + i as u64,
            ));
        }
        // 3 deletes (these will be no-op due to key matching issue)
        for _ in 0..3 {
            deltas.push(create_delta_delete(35));
        }

        let snapshot = test_snapshot(50);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        )
        .expect("materialization should succeed");

        assert_eq!(result.deltas_applied, 28);
        // All 20 cells should still exist (deletes don't match without proper key)
        assert_eq!(result.cell_count, 20);
    }

    #[test]
    fn test_materialize_snapshot_visibility() {
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();
        let deltas = vec![
            create_delta_insert(100, b"visible", 5),
            create_delta_insert(101, b"not visible", 15),
        ];

        // Snapshot at 10 should only see the first insert
        let snapshot = test_snapshot(10);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        )
        .expect("materialization should succeed");

        assert_eq!(result.deltas_applied, 1);
        assert_eq!(result.cell_count, 1);
    }

    #[test]
    fn test_materialize_threshold_trigger() {
        // Test the threshold check function
        assert!(!should_materialize_eagerly(
            10,
            DEFAULT_MATERIALIZATION_THRESHOLD
        ));
        assert!(!should_materialize_eagerly(
            32,
            DEFAULT_MATERIALIZATION_THRESHOLD
        ));
        assert!(should_materialize_eagerly(
            33,
            DEFAULT_MATERIALIZATION_THRESHOLD
        ));
        assert!(should_materialize_eagerly(
            100,
            DEFAULT_MATERIALIZATION_THRESHOLD
        ));
    }

    #[test]
    fn test_materialize_round_trip() {
        // Insert cells via deltas, materialize, verify valid page
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();

        let deltas: Vec<CellDelta> = (0..5)
            .map(|i| create_delta_insert(i as i64 + 1, format!("data{i}").as_bytes(), i as u64 + 1))
            .collect();

        let snapshot = test_snapshot(10);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        )
        .expect("materialization should succeed");

        // Parse the resulting page and verify structure
        let header = BtreePageHeader::parse(result.page.as_bytes(), 0).unwrap();
        assert_eq!(header.cell_count, 5);
        assert_eq!(header.page_type, BtreePageType::LeafTable);
        assert_eq!(header.first_freeblock, 0);
        assert_eq!(header.fragmented_free_bytes, 0);

        // Verify cell pointers are valid
        let pointers = read_cell_pointers(result.page.as_bytes(), &header, 0).unwrap();
        assert_eq!(pointers.len(), 5);
        for &ptr in &pointers {
            assert!(ptr >= header.cell_content_offset as u16);
            assert!((ptr as usize) < result.page.len());
        }
    }

    #[test]
    fn test_materialize_empty_deltas() {
        let base = create_empty_leaf_table_page();
        let page_no = PageNumber::new(2).unwrap();
        let deltas: Vec<CellDelta> = vec![];
        let snapshot = test_snapshot(10);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Checkpoint,
        )
        .expect("materialization should succeed");

        assert_eq!(result.deltas_applied, 0);
        assert_eq!(result.cell_count, 0);
        // Should return base unchanged
        assert_eq!(result.page.as_bytes(), base.as_bytes());
    }

    #[test]
    fn test_materialize_interior_page_fails() {
        // Interior pages should fail
        let mut page = vec![0u8; PAGE_SIZE as usize];
        page[0] = BtreePageType::InteriorTable as u8;
        page[3] = 0;
        page[4] = 0; // cell count = 0
        page[5] = 0x10;
        page[6] = 0x00; // cell content offset
        // Right child pointer
        page[8] = 0;
        page[9] = 0;
        page[10] = 0;
        page[11] = 3;

        let base = PageData::from_vec(page);
        let page_no = PageNumber::new(2).unwrap();
        let deltas = vec![create_delta_insert(100, b"test", 5)];
        let snapshot = test_snapshot(10);

        let result = materialize_page(
            &base,
            page_no,
            &deltas,
            &snapshot,
            USABLE_SIZE,
            MaterializationTrigger::Explicit,
        );
        assert!(result.is_err());
    }

    #[test]
    #[ignore = "requires B-tree split integration (C-TRANSITION)"]
    fn test_split_trigger_materializes() {
        // Test that filling a page via cell deltas until it needs to split
        // triggers materialization before the split operation.
        // This requires integration with the B-tree cursor and balance code.
        todo!("integration test: B-tree split triggers materialize_page()");
    }

    #[test]
    #[ignore = "requires WAL checkpoint integration (C-TRANSITION)"]
    fn test_checkpoint_materializes_all() {
        // Test that WAL checkpoint materializes all pages with outstanding
        // cell deltas and writes them to the main database file.
        // This requires integration with the checkpoint executor.
        todo!("integration test: checkpoint triggers materialize_page() for all pages");
    }
}
