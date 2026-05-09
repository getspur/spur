//! Cell-Delta WAL Frame Format (C4-WAL: bd-l9k8e.10)
//!
//! This module defines the WAL record format for cell-level MVCC deltas, enabling
//! crash recovery of committed cell-level changes without writing full 4KB page images.
//!
//! # Design Rationale
//!
//! Cell-level deltas live in memory ([`crate::cell_visibility::CellVisibilityLog`]).
//! When a transaction commits, we need durability without the cost of full page images.
//! Cell-delta WAL frames are ~28-30x smaller than full-page frames for typical rows.
//!
//! # Frame Format
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -----
//!   0       1   Frame type byte (0x43 = 'C' for cell-delta)
//!   1       4   Page number (big-endian)
//!   5      16   Cell key digest (BLAKE3 truncated)
//!  21       1   Cell operation (1=Insert, 2=Update, 3=Delete)
//!  22       8   Commit sequence (big-endian)
//!  30       8   Transaction ID (big-endian)
//!  38       4   Cell data length (big-endian, 0 for Delete)
//!  42       N   Cell data bytes
//!  42+N     4   CRC32C checksum of bytes 0..(42+N)
//! ```
//!
//! Total overhead: 46 bytes fixed + cell_data
//! Typical 100-byte INSERT: ~146 bytes vs 4096 bytes full-page = ~28x smaller
//!
//! # Frame Type Discrimination
//!
//! Regular full-page frames start with page_number (4 bytes, always >= 1 for valid pages).
//! Cell-delta frames start with type byte 0x43 ('C'), which would decode as page_number
//! 0x43000000 (1124073472) if misinterpreted. This is distinguishable by checking:
//! - If byte[0] == CELL_DELTA_FRAME_TYPE, it's a cell-delta frame
//! - Otherwise, interpret as regular full-page frame
//!
//! # Recovery
//!
//! During WAL recovery:
//! 1. Read frame type byte
//! 2. Full-page frames: apply directly to page cache (existing path)
//! 3. Cell-delta frames: insert into [`CellVisibilityLog`], then materialize affected pages
//!
//! # Checkpoint Integration
//!
//! At checkpoint:
//! 1. Materialize all pages with outstanding cell deltas
//! 2. Write full page images to main DB file
//! 3. Truncate WAL (clears both frame types)
//! 4. Clear [`CellVisibilityLog`] for checkpointed pages

use fsqlite_types::{CommitSeq, PageNumber, TxnId};
use tracing::{debug, trace, warn};

use crate::cell_visibility::{CellDeltaKind, CellKey};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Frame type marker for cell-delta frames.
/// 'C' = 0x43, chosen to be distinguishable from valid page numbers (>= 1).
pub const CELL_DELTA_FRAME_TYPE: u8 = 0x43;

/// Fixed header size before variable-length cell data.
pub const CELL_DELTA_HEADER_SIZE: usize = 42;

/// CRC32C checksum size.
pub const CELL_DELTA_CHECKSUM_SIZE: usize = 4;

/// Minimum frame size (header + checksum, no data).
pub const CELL_DELTA_MIN_FRAME_SIZE: usize = CELL_DELTA_HEADER_SIZE + CELL_DELTA_CHECKSUM_SIZE;

/// Maximum cell data length (practical limit to prevent pathological frames).
pub const CELL_DELTA_MAX_DATA_LEN: u32 = 1024 * 1024; // 1MB

// ---------------------------------------------------------------------------
// CellDeltaOp — Wire format for cell operation kind
// ---------------------------------------------------------------------------

/// Cell operation encoded as a single byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CellDeltaOp {
    Insert = 1,
    Update = 2,
    Delete = 3,
}

impl CellDeltaOp {
    /// Convert from wire byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Insert),
            2 => Some(Self::Update),
            3 => Some(Self::Delete),
            _ => None,
        }
    }

    /// Convert from [`CellDeltaKind`].
    #[must_use]
    pub fn from_kind(kind: &CellDeltaKind) -> Self {
        match kind {
            CellDeltaKind::Insert => Self::Insert,
            CellDeltaKind::Update => Self::Update,
            CellDeltaKind::Delete => Self::Delete,
        }
    }

    /// Convert to [`CellDeltaKind`].
    #[must_use]
    pub fn to_kind(self) -> CellDeltaKind {
        match self {
            Self::Insert => CellDeltaKind::Insert,
            Self::Update => CellDeltaKind::Update,
            Self::Delete => CellDeltaKind::Delete,
        }
    }
}

// ---------------------------------------------------------------------------
// CellDeltaWalFrame — The cell-delta WAL record
// ---------------------------------------------------------------------------

/// A cell-delta WAL frame for crash recovery.
///
/// This is the lightweight alternative to full-page WAL frames for logical
/// row operations (INSERT/UPDATE/DELETE that don't trigger structural changes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellDeltaWalFrame {
    /// Page number containing this cell.
    pub page_number: PageNumber,
    /// BLAKE3-truncated digest of the cell key.
    pub key_digest: [u8; 16],
    /// What operation was performed.
    pub op: CellDeltaOp,
    /// Commit sequence when this delta became visible.
    pub commit_seq: CommitSeq,
    /// Transaction that created this delta.
    pub txn_id: TxnId,
    /// Cell data bytes (empty for Delete).
    pub cell_data: Vec<u8>,
}

impl CellDeltaWalFrame {
    /// Create a new cell-delta WAL frame.
    #[must_use]
    pub fn new(
        page_number: PageNumber,
        cell_key: &CellKey,
        op: CellDeltaOp,
        commit_seq: CommitSeq,
        txn_id: TxnId,
        cell_data: Vec<u8>,
    ) -> Self {
        Self {
            page_number,
            key_digest: cell_key.key_digest,
            op,
            commit_seq,
            txn_id,
            cell_data,
        }
    }

    /// Total serialized size of this frame.
    #[must_use]
    pub fn serialized_size(&self) -> usize {
        CELL_DELTA_HEADER_SIZE + self.cell_data.len() + CELL_DELTA_CHECKSUM_SIZE
    }

    /// Serialize this frame to bytes.
    ///
    /// Returns the complete frame including CRC32C checksum.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let total_size = self.serialized_size();
        let mut buf = Vec::with_capacity(total_size);

        // Frame type byte
        buf.push(CELL_DELTA_FRAME_TYPE);

        // Page number (4 bytes, big-endian)
        buf.extend_from_slice(&self.page_number.get().to_be_bytes());

        // Key digest (16 bytes)
        buf.extend_from_slice(&self.key_digest);

        // Operation (1 byte)
        buf.push(self.op as u8);

        // Commit sequence (8 bytes, big-endian)
        buf.extend_from_slice(&self.commit_seq.get().to_be_bytes());

        // Transaction ID (8 bytes, big-endian)
        buf.extend_from_slice(&self.txn_id.get().to_be_bytes());

        // Cell data length (4 bytes, big-endian)
        let data_len = u32::try_from(self.cell_data.len()).unwrap_or(u32::MAX);
        buf.extend_from_slice(&data_len.to_be_bytes());

        // Cell data
        buf.extend_from_slice(&self.cell_data);

        // CRC32C checksum of everything before the checksum
        let checksum = crc32c_checksum(&buf);
        buf.extend_from_slice(&checksum.to_be_bytes());

        trace!(
            pgno = self.page_number.get(),
            op = ?self.op,
            commit_seq = self.commit_seq.get(),
            data_len = self.cell_data.len(),
            frame_size = buf.len(),
            "cell_delta_wal_frame_serialized"
        );

        buf
    }

    /// Deserialize a cell-delta frame from bytes.
    ///
    /// Returns `None` if:
    /// - Frame is too short
    /// - Frame type byte doesn't match
    /// - CRC32C checksum fails
    /// - Cell data length exceeds maximum
    #[must_use]
    pub fn deserialize(buf: &[u8]) -> Option<Self> {
        if buf.len() < CELL_DELTA_MIN_FRAME_SIZE {
            warn!(
                buf_len = buf.len(),
                min_size = CELL_DELTA_MIN_FRAME_SIZE,
                "cell_delta_wal_frame_too_short"
            );
            return None;
        }

        // Check frame type
        if buf[0] != CELL_DELTA_FRAME_TYPE {
            return None; // Not a cell-delta frame
        }

        // Read header fields
        let page_number = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
        let page_number = PageNumber::new(page_number)?;

        let mut key_digest = [0u8; 16];
        key_digest.copy_from_slice(&buf[5..21]);

        let op = CellDeltaOp::from_byte(buf[21])?;

        let commit_seq = u64::from_be_bytes([
            buf[22], buf[23], buf[24], buf[25], buf[26], buf[27], buf[28], buf[29],
        ]);
        let commit_seq = CommitSeq::new(commit_seq);

        let txn_id = u64::from_be_bytes([
            buf[30], buf[31], buf[32], buf[33], buf[34], buf[35], buf[36], buf[37],
        ]);
        let txn_id = TxnId::new(txn_id)?;

        let data_len = u32::from_be_bytes([buf[38], buf[39], buf[40], buf[41]]);

        // Validate data length
        if data_len > CELL_DELTA_MAX_DATA_LEN {
            warn!(
                data_len,
                max = CELL_DELTA_MAX_DATA_LEN,
                "cell_delta_wal_frame_data_too_large"
            );
            return None;
        }

        let expected_total_size =
            CELL_DELTA_HEADER_SIZE + data_len as usize + CELL_DELTA_CHECKSUM_SIZE;
        if buf.len() < expected_total_size {
            warn!(
                buf_len = buf.len(),
                expected_size = expected_total_size,
                "cell_delta_wal_frame_truncated"
            );
            return None;
        }

        // Extract cell data
        let data_start = CELL_DELTA_HEADER_SIZE;
        let data_end = data_start + data_len as usize;
        let cell_data = buf[data_start..data_end].to_vec();

        // Verify checksum
        let checksum_start = data_end;
        let stored_checksum = u32::from_be_bytes([
            buf[checksum_start],
            buf[checksum_start + 1],
            buf[checksum_start + 2],
            buf[checksum_start + 3],
        ]);
        let computed_checksum = crc32c_checksum(&buf[..checksum_start]);

        if stored_checksum != computed_checksum {
            warn!(
                stored = stored_checksum,
                computed = computed_checksum,
                "cell_delta_wal_frame_checksum_mismatch"
            );
            return None;
        }

        trace!(
            pgno = page_number.get(),
            op = ?op,
            commit_seq = commit_seq.get(),
            data_len,
            "cell_delta_wal_frame_deserialized"
        );

        Some(Self {
            page_number,
            key_digest,
            op,
            commit_seq,
            txn_id,
            cell_data,
        })
    }

    /// Check if a buffer starts with the cell-delta frame type marker.
    #[inline]
    #[must_use]
    pub fn is_cell_delta_frame(buf: &[u8]) -> bool {
        !buf.is_empty() && buf[0] == CELL_DELTA_FRAME_TYPE
    }
}

// ---------------------------------------------------------------------------
// CRC32C Checksum
// ---------------------------------------------------------------------------

/// Compute CRC32C checksum of data using the crc32c crate.
#[inline]
fn crc32c_checksum(data: &[u8]) -> u32 {
    crc32c::crc32c(data)
}

// ---------------------------------------------------------------------------
// Batch Serialization
// ---------------------------------------------------------------------------

/// Serialize multiple cell-delta frames into a single buffer.
///
/// This is used when a transaction commits multiple cell changes atomically.
/// Each frame is written sequentially with its own checksum.
#[must_use]
pub fn serialize_cell_delta_batch(frames: &[CellDeltaWalFrame]) -> Vec<u8> {
    let total_size: usize = frames.iter().map(CellDeltaWalFrame::serialized_size).sum();
    let mut buf = Vec::with_capacity(total_size);

    for frame in frames {
        buf.extend_from_slice(&frame.serialize());
    }

    debug!(
        frame_count = frames.len(),
        total_bytes = buf.len(),
        "cell_delta_batch_serialized"
    );

    buf
}

/// Deserialize cell-delta frames from a buffer.
///
/// Reads frames sequentially until the buffer is exhausted or a non-cell-delta
/// frame is encountered.
#[must_use]
pub fn deserialize_cell_delta_batch(buf: &[u8]) -> Vec<CellDeltaWalFrame> {
    let mut frames = Vec::new();
    let mut offset = 0;

    while offset < buf.len() {
        let remaining = &buf[offset..];

        // Check if this looks like a cell-delta frame
        if !CellDeltaWalFrame::is_cell_delta_frame(remaining) {
            break;
        }

        // Need at least header to read data length
        if remaining.len() < CELL_DELTA_HEADER_SIZE {
            break;
        }

        // Read data length to determine frame size
        let data_len =
            u32::from_be_bytes([remaining[38], remaining[39], remaining[40], remaining[41]]);

        let frame_size = CELL_DELTA_HEADER_SIZE + data_len as usize + CELL_DELTA_CHECKSUM_SIZE;

        if remaining.len() < frame_size {
            break;
        }

        // Try to deserialize
        if let Some(frame) = CellDeltaWalFrame::deserialize(&remaining[..frame_size]) {
            frames.push(frame);
            offset += frame_size;
        } else {
            break;
        }
    }

    debug!(
        frame_count = frames.len(),
        bytes_consumed = offset,
        "cell_delta_batch_deserialized"
    );

    frames
}

// ---------------------------------------------------------------------------
// Recovery Summary
// ---------------------------------------------------------------------------

/// Summary statistics from WAL recovery.
#[derive(Debug, Clone, Default)]
pub struct CellDeltaRecoverySummary {
    /// Number of cell-delta frames recovered.
    pub cell_delta_frames: u64,
    /// Number of full-page frames recovered.
    pub full_page_frames: u64,
    /// Total bytes in cell-delta frames.
    pub cell_delta_bytes: u64,
    /// Number of unique pages with cell deltas.
    pub pages_with_cell_deltas: u64,
    /// Number of cell deltas inserted into the visibility log.
    pub deltas_inserted: u64,
}

impl CellDeltaRecoverySummary {
    /// Log the recovery summary.
    pub fn log_summary(&self) {
        tracing::info!(
            cell_delta_frames = self.cell_delta_frames,
            full_page_frames = self.full_page_frames,
            cell_delta_bytes = self.cell_delta_bytes,
            pages_with_cell_deltas = self.pages_with_cell_deltas,
            deltas_inserted = self.deltas_inserted,
            "wal_recovery_summary"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use fsqlite_types::{BtreeRef, SemanticKeyKind, TableId};

    fn make_cell_key() -> CellKey {
        CellKey {
            btree: BtreeRef::Table(TableId::new(1)),
            kind: SemanticKeyKind::TableRow,
            key_digest: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        }
    }

    // -----------------------------------------------------------------------
    // Serialization tests (from C4-WAL bead)
    // -----------------------------------------------------------------------

    #[test]
    fn test_cell_delta_frame_round_trip() {
        let frame = CellDeltaWalFrame {
            page_number: PageNumber::new(42).unwrap(),
            key_digest: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            op: CellDeltaOp::Insert,
            commit_seq: CommitSeq::new(12345),
            txn_id: TxnId::new(67890).unwrap(),
            cell_data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };

        let serialized = frame.serialize();
        let deserialized = CellDeltaWalFrame::deserialize(&serialized);

        assert_eq!(deserialized, Some(frame));
    }

    #[test]
    fn test_cell_delta_frame_checksum() {
        let frame = CellDeltaWalFrame {
            page_number: PageNumber::new(42).unwrap(),
            key_digest: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            op: CellDeltaOp::Update,
            commit_seq: CommitSeq::new(100),
            txn_id: TxnId::new(200).unwrap(),
            cell_data: vec![1, 2, 3, 4, 5],
        };

        let mut serialized = frame.serialize();

        // Corrupt one byte in the middle
        let corrupt_idx = serialized.len() / 2;
        serialized[corrupt_idx] ^= 0xFF;

        // Should fail to deserialize
        assert!(CellDeltaWalFrame::deserialize(&serialized).is_none());
    }

    #[test]
    fn test_cell_delta_frame_variable_length() {
        // Empty cell data (Delete)
        let frame_empty = CellDeltaWalFrame {
            page_number: PageNumber::new(1).unwrap(),
            key_digest: [0; 16],
            op: CellDeltaOp::Delete,
            commit_seq: CommitSeq::new(1),
            txn_id: TxnId::new(1).unwrap(),
            cell_data: vec![],
        };
        let ser = frame_empty.serialize();
        assert_eq!(CellDeltaWalFrame::deserialize(&ser), Some(frame_empty));

        // 100 bytes cell data
        let frame_100 = CellDeltaWalFrame {
            page_number: PageNumber::new(2).unwrap(),
            key_digest: [1; 16],
            op: CellDeltaOp::Insert,
            commit_seq: CommitSeq::new(2),
            txn_id: TxnId::new(2).unwrap(),
            cell_data: vec![0xAB; 100],
        };
        let ser = frame_100.serialize();
        assert_eq!(CellDeltaWalFrame::deserialize(&ser), Some(frame_100));

        // 4000 bytes cell data
        let frame_4000 = CellDeltaWalFrame {
            page_number: PageNumber::new(3).unwrap(),
            key_digest: [2; 16],
            op: CellDeltaOp::Update,
            commit_seq: CommitSeq::new(3),
            txn_id: TxnId::new(3).unwrap(),
            cell_data: vec![0xCD; 4000],
        };
        let ser = frame_4000.serialize();
        assert_eq!(CellDeltaWalFrame::deserialize(&ser), Some(frame_4000));
    }

    #[test]
    fn test_cell_delta_frame_type_byte() {
        let frame = CellDeltaWalFrame {
            page_number: PageNumber::new(42).unwrap(),
            key_digest: [0; 16],
            op: CellDeltaOp::Insert,
            commit_seq: CommitSeq::new(1),
            txn_id: TxnId::new(1).unwrap(),
            cell_data: vec![1, 2, 3],
        };

        let serialized = frame.serialize();

        // First byte should be the type marker
        assert_eq!(serialized[0], CELL_DELTA_FRAME_TYPE);

        // is_cell_delta_frame should return true
        assert!(CellDeltaWalFrame::is_cell_delta_frame(&serialized));

        // Regular page frame (starts with page number) should return false
        let fake_page_frame = [0x00, 0x00, 0x00, 0x01]; // page 1
        assert!(!CellDeltaWalFrame::is_cell_delta_frame(&fake_page_frame));
    }

    #[test]
    fn test_cell_delta_op_conversion() {
        assert_eq!(CellDeltaOp::from_byte(1), Some(CellDeltaOp::Insert));
        assert_eq!(CellDeltaOp::from_byte(2), Some(CellDeltaOp::Update));
        assert_eq!(CellDeltaOp::from_byte(3), Some(CellDeltaOp::Delete));
        assert_eq!(CellDeltaOp::from_byte(0), None);
        assert_eq!(CellDeltaOp::from_byte(4), None);
        assert_eq!(CellDeltaOp::from_byte(255), None);
    }

    #[test]
    fn test_batch_serialization() {
        let frames = vec![
            CellDeltaWalFrame {
                page_number: PageNumber::new(1).unwrap(),
                key_digest: [1; 16],
                op: CellDeltaOp::Insert,
                commit_seq: CommitSeq::new(10),
                txn_id: TxnId::new(100).unwrap(),
                cell_data: vec![1, 2, 3],
            },
            CellDeltaWalFrame {
                page_number: PageNumber::new(2).unwrap(),
                key_digest: [2; 16],
                op: CellDeltaOp::Update,
                commit_seq: CommitSeq::new(20),
                txn_id: TxnId::new(200).unwrap(),
                cell_data: vec![4, 5, 6, 7],
            },
            CellDeltaWalFrame {
                page_number: PageNumber::new(3).unwrap(),
                key_digest: [3; 16],
                op: CellDeltaOp::Delete,
                commit_seq: CommitSeq::new(30),
                txn_id: TxnId::new(300).unwrap(),
                cell_data: vec![],
            },
        ];

        let serialized = serialize_cell_delta_batch(&frames);
        let deserialized = deserialize_cell_delta_batch(&serialized);

        assert_eq!(deserialized, frames);
    }

    #[test]
    fn test_serialized_size() {
        // Empty data: 42 header + 0 data + 4 checksum = 46
        let frame_empty = CellDeltaWalFrame {
            page_number: PageNumber::new(1).unwrap(),
            key_digest: [0; 16],
            op: CellDeltaOp::Delete,
            commit_seq: CommitSeq::new(1),
            txn_id: TxnId::new(1).unwrap(),
            cell_data: vec![],
        };
        assert_eq!(frame_empty.serialized_size(), 46);
        assert_eq!(frame_empty.serialize().len(), 46);

        // 100 bytes data: 42 + 100 + 4 = 146
        let frame_100 = CellDeltaWalFrame {
            page_number: PageNumber::new(1).unwrap(),
            key_digest: [0; 16],
            op: CellDeltaOp::Insert,
            commit_seq: CommitSeq::new(1),
            txn_id: TxnId::new(1).unwrap(),
            cell_data: vec![0; 100],
        };
        assert_eq!(frame_100.serialized_size(), 146);
        assert_eq!(frame_100.serialize().len(), 146);
    }

    #[test]
    fn test_truncated_frame_rejected() {
        let frame = CellDeltaWalFrame {
            page_number: PageNumber::new(42).unwrap(),
            key_digest: [0; 16],
            op: CellDeltaOp::Insert,
            commit_seq: CommitSeq::new(1),
            txn_id: TxnId::new(1).unwrap(),
            cell_data: vec![1, 2, 3, 4, 5],
        };

        let serialized = frame.serialize();

        // Truncate at various points
        for truncate_at in [0, 10, 20, 40, serialized.len() - 1] {
            let truncated = &serialized[..truncate_at];
            assert!(
                CellDeltaWalFrame::deserialize(truncated).is_none(),
                "Should reject frame truncated at {truncate_at}"
            );
        }
    }

    #[test]
    fn test_invalid_page_number_rejected() {
        // page_number 0 is invalid
        let mut buf = vec![CELL_DELTA_FRAME_TYPE];
        buf.extend_from_slice(&0u32.to_be_bytes()); // page 0
        buf.extend_from_slice(&[0u8; 16]); // key_digest
        buf.push(1); // op
        buf.extend_from_slice(&1u64.to_be_bytes()); // commit_seq
        buf.extend_from_slice(&1u64.to_be_bytes()); // txn_id
        buf.extend_from_slice(&0u32.to_be_bytes()); // data_len
        let checksum = crc32c_checksum(&buf);
        buf.extend_from_slice(&checksum.to_be_bytes());

        assert!(CellDeltaWalFrame::deserialize(&buf).is_none());
    }

    #[test]
    fn test_invalid_txn_id_rejected() {
        // txn_id 0 is invalid
        let mut buf = vec![CELL_DELTA_FRAME_TYPE];
        buf.extend_from_slice(&1u32.to_be_bytes()); // page 1
        buf.extend_from_slice(&[0u8; 16]); // key_digest
        buf.push(1); // op
        buf.extend_from_slice(&1u64.to_be_bytes()); // commit_seq
        buf.extend_from_slice(&0u64.to_be_bytes()); // txn_id 0 (invalid)
        buf.extend_from_slice(&0u32.to_be_bytes()); // data_len
        let checksum = crc32c_checksum(&buf);
        buf.extend_from_slice(&checksum.to_be_bytes());

        assert!(CellDeltaWalFrame::deserialize(&buf).is_none());
    }

    #[test]
    fn test_invalid_op_rejected() {
        let mut buf = vec![CELL_DELTA_FRAME_TYPE];
        buf.extend_from_slice(&1u32.to_be_bytes()); // page 1
        buf.extend_from_slice(&[0u8; 16]); // key_digest
        buf.push(99); // invalid op
        buf.extend_from_slice(&1u64.to_be_bytes()); // commit_seq
        buf.extend_from_slice(&1u64.to_be_bytes()); // txn_id
        buf.extend_from_slice(&0u32.to_be_bytes()); // data_len
        let checksum = crc32c_checksum(&buf);
        buf.extend_from_slice(&checksum.to_be_bytes());

        assert!(CellDeltaWalFrame::deserialize(&buf).is_none());
    }

    #[test]
    fn test_from_cell_key() {
        let cell_key = make_cell_key();
        let frame = CellDeltaWalFrame::new(
            PageNumber::new(42).unwrap(),
            &cell_key,
            CellDeltaOp::Insert,
            CommitSeq::new(100),
            TxnId::new(200).unwrap(),
            vec![1, 2, 3],
        );

        assert_eq!(frame.key_digest, cell_key.key_digest);
    }
}
