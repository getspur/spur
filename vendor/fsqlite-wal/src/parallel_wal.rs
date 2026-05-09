//! Parallel WAL coordinator (D1: bd-3wop3.1).
//!
//! This module provides a lock-free parallel WAL write path using per-thread
//! buffers and epoch-based group commit. It replaces the global WAL append
//! mutex with cooperative per-thread buffering.
//!
//! # Architecture
//!
//! 1. Each writer thread appends WAL frames to its own buffer with NO global lock.
//! 2. A background epoch ticker advances the global epoch every ~10ms.
//! 3. On epoch advance, slot-local buffer locks make sealing wait for any
//!    in-flight batch append to complete, then the previous epoch is flushed.
//! 4. Commit durability: transaction waits until its epoch is durable.
//!
//! # Key Benefits
//!
//! - Eliminates the #1 contention point (global WAL append mutex).
//! - WAL writes are now embarrassingly parallel.
//! - Epoch mechanism provides natural group commit semantics (Silo/Aether pattern).

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fsqlite_types::{CommitSeq, PageNumber, TxnToken};

use crate::per_core_buffer::{
    AppendOutcome, BufferConfig, DEFAULT_BUFFER_SLOT_COUNT, EpochConfig, EpochFlushBatch,
    EpochOrderCoordinator, WalRecord, thread_buffer_slot,
};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the parallel WAL coordinator.
#[derive(Debug, Clone, Copy)]
pub struct ParallelWalConfig {
    /// Number of buffer slots (typically 128 for 16 threads).
    pub slot_count: usize,
    /// Epoch advance interval in milliseconds (default: 10ms).
    pub epoch_interval_ms: u64,
    /// Buffer capacity in bytes per slot (default: 4MB).
    pub buffer_capacity_bytes: usize,
}

impl Default for ParallelWalConfig {
    fn default() -> Self {
        Self {
            slot_count: DEFAULT_BUFFER_SLOT_COUNT,
            epoch_interval_ms: 10,
            buffer_capacity_bytes: 4 * 1024 * 1024,
        }
    }
}

// ---------------------------------------------------------------------------
// Segment File I/O (D1.6)
// ---------------------------------------------------------------------------

/// Magic number for parallel WAL segment files.
const SEGMENT_MAGIC: u32 = 0x5057_414C; // "PWAL"

/// Version of the segment file format.
const SEGMENT_VERSION: u16 = 1;

/// Segment file header size in bytes.
const SEGMENT_HEADER_SIZE: usize = 24;

/// fsync policy for segment files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsyncPolicy {
    /// Full fsync after every write (safest, slowest).
    #[default]
    Full,
    /// Fsync at epoch boundaries only.
    Normal,
    /// No fsync (fastest, least safe).
    Off,
}

/// Segment file header.
///
/// Layout (24 bytes):
/// ```text
/// [0..4]   magic: u32 (0x5057414C = "PWAL")
/// [4..6]   version: u16
/// [6..8]   reserved: u16 (for alignment)
/// [8..16]  epoch: u64
/// [16..20] record_count: u32
/// [20..24] checksum: u32 (CRC32C of header fields 0..20)
/// ```
#[derive(Debug, Clone, Copy)]
pub struct SegmentHeader {
    /// Epoch number for this segment.
    pub epoch: u64,
    /// Number of records in this segment.
    pub record_count: u32,
}

impl SegmentHeader {
    /// Create a new segment header.
    #[must_use]
    pub const fn new(epoch: u64, record_count: u32) -> Self {
        Self {
            epoch,
            record_count,
        }
    }

    /// Serialize the header to bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SEGMENT_HEADER_SIZE] {
        let mut buf = [0u8; SEGMENT_HEADER_SIZE];
        buf[0..4].copy_from_slice(&SEGMENT_MAGIC.to_le_bytes());
        buf[4..6].copy_from_slice(&SEGMENT_VERSION.to_le_bytes());
        // buf[6..8] reserved
        buf[8..16].copy_from_slice(&self.epoch.to_le_bytes());
        buf[16..20].copy_from_slice(&self.record_count.to_le_bytes());
        // Compute CRC32C of bytes 0..20
        let checksum = crc32c::crc32c(&buf[0..20]);
        buf[20..24].copy_from_slice(&checksum.to_le_bytes());
        buf
    }

    /// Parse a header from bytes.
    pub fn from_bytes(buf: &[u8; SEGMENT_HEADER_SIZE]) -> Result<Self, String> {
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != SEGMENT_MAGIC {
            return Err(format!("invalid segment magic: {magic:#x}"));
        }
        let version = u16::from_le_bytes([buf[4], buf[5]]);
        if version != SEGMENT_VERSION {
            return Err(format!("unsupported segment version: {version}"));
        }
        let epoch = u64::from_le_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
        ]);
        let record_count = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let stored_checksum = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let computed_checksum = crc32c::crc32c(&buf[0..20]);
        if stored_checksum != computed_checksum {
            return Err(format!(
                "segment header checksum mismatch: stored={stored_checksum:#x}, computed={computed_checksum:#x}"
            ));
        }
        Ok(Self {
            epoch,
            record_count,
        })
    }
}

/// Generate the segment file path for a given database and epoch.
#[must_use]
pub fn segment_path(db_path: &Path, epoch: u64) -> PathBuf {
    let mut path = db_path.to_path_buf();
    let file_name = path
        .file_name()
        .map_or_else(|| "db".to_string(), |n| n.to_string_lossy().to_string());
    path.set_file_name(format!("{file_name}-wal-seg-{epoch:016x}"));
    path
}

/// List all segment files for a database, sorted by epoch.
pub fn list_segments(db_path: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let dir = db_path.parent().unwrap_or_else(|| Path::new("."));
    let db_name = db_path
        .file_name()
        .map_or_else(|| "db".to_string(), |n| n.to_string_lossy().to_string());
    let prefix = format!("{db_name}-wal-seg-");

    let mut segments = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(epoch_hex) = name_str.strip_prefix(&prefix) {
            if let Ok(epoch) = u64::from_str_radix(epoch_hex, 16) {
                segments.push((epoch, entry.path()));
            }
        }
    }
    segments.sort_by_key(|(epoch, _)| *epoch);
    Ok(segments)
}

/// Write a segment file for the given epoch batch.
///
/// The segment file contains:
/// 1. Header with epoch and record count
/// 2. Serialized records (length-prefixed bincode)
///
/// Returns the number of bytes written.
pub fn write_segment(
    db_path: &Path,
    batch: &EpochFlushBatch,
    fsync_policy: FsyncPolicy,
) -> io::Result<usize> {
    let path = segment_path(db_path, batch.epoch);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    let mut writer = BufWriter::new(file);

    let ordered_records = ordered_segment_records(batch.epoch, &batch.records)?;

    // Write header
    let header = SegmentHeader::new(batch.epoch, ordered_records.len() as u32);
    let header_bytes = header.to_bytes();
    writer.write_all(&header_bytes)?;
    let mut total_bytes = SEGMENT_HEADER_SIZE;

    // Write records in canonical replay order so crash recovery is deterministic.
    for record in &ordered_records {
        let record_bytes = serialize_record(record);
        let len = record_bytes.len() as u32;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&record_bytes)?;
        total_bytes += 4 + record_bytes.len();
    }

    writer.flush()?;

    // Apply fsync policy
    if fsync_policy == FsyncPolicy::Full || fsync_policy == FsyncPolicy::Normal {
        writer.get_ref().sync_all()?;
    }

    Ok(total_bytes)
}

/// Read a segment file and return the records.
pub fn read_segment(path: &Path) -> io::Result<(SegmentHeader, Vec<WalRecord>)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    // Read header
    let mut header_buf = [0u8; SEGMENT_HEADER_SIZE];
    reader.read_exact(&mut header_buf)?;
    let header = SegmentHeader::from_bytes(&header_buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Read records
    let mut records = Vec::with_capacity(header.record_count as usize);
    for _ in 0..header.record_count {
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        let mut record_buf = vec![0u8; len];
        reader.read_exact(&mut record_buf)?;
        let record = deserialize_record(&record_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        records.push(record);
    }

    Ok((header, ordered_segment_records(header.epoch, &records)?))
}

/// Delete a segment file.
pub fn delete_segment(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

// ---------------------------------------------------------------------------
// Segment Recovery (D1.7)
// ---------------------------------------------------------------------------

/// Result of recovering segments for a database.
#[derive(Debug, Clone)]
pub struct SegmentRecoveryResult {
    /// Number of segments recovered.
    pub segments_recovered: usize,
    /// Number of records applied.
    pub records_applied: usize,
    /// Total bytes read from segment files.
    pub bytes_read: u64,
    /// Epochs recovered, in order.
    pub epochs: Vec<u64>,
    /// Any partial segments that were skipped (truncated/corrupt).
    pub partial_segments: Vec<PathBuf>,
}

/// Options for segment recovery.
#[derive(Debug, Clone, Copy, Default)]
pub struct SegmentRecoveryOptions {
    /// Delete segment files after successful recovery.
    pub delete_after_recovery: bool,
    /// Stop at the first corrupt segment and return the durable prefix instead
    /// of failing the whole recovery.
    pub skip_corrupt: bool,
}

/// Recover all segments for a database.
///
/// This function:
/// 1. Finds all segment files for the database.
/// 2. Sorts them by epoch (ascending).
/// 3. Reads and returns records from each segment.
/// 4. Optionally deletes segments after recovery.
///
/// The caller is responsible for applying records to the database
/// (updating page contents based on after_images).
pub fn recover_segments(
    db_path: &Path,
    options: SegmentRecoveryOptions,
) -> io::Result<(SegmentRecoveryResult, Vec<WalRecord>)> {
    let segments = list_segments(db_path)?;

    let mut result = SegmentRecoveryResult {
        segments_recovered: 0,
        records_applied: 0,
        bytes_read: 0,
        epochs: Vec::with_capacity(segments.len()),
        partial_segments: Vec::new(),
    };

    let mut all_records = Vec::new();

    for (segment_index, (epoch, path)) in segments.iter().enumerate() {
        // Get file size for byte tracking
        let metadata = fs::metadata(path)?;
        let file_size = metadata.len();

        // Try to read the segment
        match read_segment(path) {
            Ok((header, records)) => {
                if header.epoch != *epoch {
                    let error = io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "segment {} has mismatched epoch: header={}, filename={}",
                            path.display(),
                            header.epoch,
                            epoch
                        ),
                    );
                    if options.skip_corrupt {
                        eprintln!(
                            "warning: stopping recovery at corrupt segment {}: {error}",
                            path.display()
                        );
                        result.partial_segments.extend(
                            segments[segment_index..]
                                .iter()
                                .map(|(_, path)| path.clone()),
                        );
                        break;
                    }
                    return Err(error);
                }

                result.segments_recovered += 1;
                result.records_applied += records.len();
                result.bytes_read += file_size;
                result.epochs.push(*epoch);

                all_records.extend(records);
            }
            Err(e) => {
                if options.skip_corrupt {
                    eprintln!(
                        "warning: stopping recovery at corrupt segment {}: {e}",
                        path.display()
                    );
                    result.partial_segments.extend(
                        segments[segment_index..]
                            .iter()
                            .map(|(_, path)| path.clone()),
                    );
                    break;
                }
                return Err(e);
            }
        }
    }

    // Delete segments after successful recovery if requested
    if options.delete_after_recovery {
        for (_, path) in &segments {
            // Skip segments that were partial/corrupt
            if result.partial_segments.contains(path) {
                continue;
            }
            if let Err(e) = delete_segment(path) {
                eprintln!("warning: failed to delete segment {}: {e}", path.display());
            }
        }
    }

    Ok((result, EpochOrderCoordinator::recovery_order(&all_records)))
}

fn ordered_segment_records(epoch: u64, records: &[WalRecord]) -> io::Result<Vec<WalRecord>> {
    let ordered = EpochOrderCoordinator::recovery_order(records);
    if let Some(record) = ordered.iter().find(|record| record.epoch != epoch) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "segment epoch {epoch} contains record from epoch {}",
                record.epoch
            ),
        ));
    }
    Ok(ordered)
}

/// Recover segments and apply records to a page cache.
///
/// This is a higher-level recovery function that takes a mutable page
/// map and applies after_images from recovered records. It returns
/// the recovery result and the final page contents.
///
/// The page_contents map is keyed by page number and contains the
/// current contents of each page. Records are applied in epoch order.
pub fn recover_and_apply_segments(
    db_path: &Path,
    page_contents: &mut HashMap<u32, Vec<u8>, impl BuildHasher>,
    options: SegmentRecoveryOptions,
) -> io::Result<SegmentRecoveryResult> {
    let (result, records) = recover_segments(db_path, options)?;

    // Apply records in order (they're already sorted by epoch)
    for record in records {
        let page_id = record.page_id.get();
        if !record.after_image.is_empty() {
            page_contents.insert(page_id, record.after_image);
        }
    }

    Ok(result)
}

/// Get the maximum durable epoch from existing segment files.
///
/// This can be used to determine the recovery point after a crash.
/// Returns None if no segment files exist.
pub fn max_durable_epoch(db_path: &Path) -> io::Result<Option<u64>> {
    let segments = list_segments(db_path)?;
    Ok(segments.last().map(|(epoch, _)| *epoch))
}

/// Clean up all segment files for a database.
///
/// This should be called after checkpoint when segments are no longer needed.
pub fn cleanup_segments(db_path: &Path) -> io::Result<usize> {
    let segments = list_segments(db_path)?;
    let count = segments.len();
    for (_, path) in segments {
        delete_segment(&path)?;
    }
    Ok(count)
}

/// Serialize a WalRecord to bytes.
fn serialize_record(record: &WalRecord) -> Vec<u8> {
    // Simple binary format:
    // [8] txn_id
    // [8] txn_epoch
    // [8] record_epoch
    // [4] page_id
    // [8] begin_seq
    // [1] has_end_seq
    // [8] end_seq (if has_end_seq)
    // [4] before_image_len
    // [N] before_image
    // [4] after_image_len
    // [N] after_image
    let mut buf = Vec::with_capacity(64 + record.before_image.len() + record.after_image.len());

    buf.extend_from_slice(&record.txn_token.id.get().to_le_bytes());
    buf.extend_from_slice(&record.txn_token.epoch.get().to_le_bytes());
    buf.extend_from_slice(&record.epoch.to_le_bytes());
    buf.extend_from_slice(&record.page_id.get().to_le_bytes());
    buf.extend_from_slice(&record.begin_seq.get().to_le_bytes());
    if let Some(end_seq) = record.end_seq {
        buf.push(1);
        buf.extend_from_slice(&end_seq.get().to_le_bytes());
    } else {
        buf.push(0);
    }
    buf.extend_from_slice(&(record.before_image.len() as u32).to_le_bytes());
    buf.extend_from_slice(&record.before_image);
    buf.extend_from_slice(&(record.after_image.len() as u32).to_le_bytes());
    buf.extend_from_slice(&record.after_image);

    buf
}

/// Deserialize a WalRecord from bytes.
fn deserialize_record(buf: &[u8]) -> Result<WalRecord, String> {
    // Minimum size: 8 + 4 + 8 + 4 + 8 + 1 + 4 + 4 = 41 bytes (no end_seq, empty images)
    if buf.len() < 41 {
        return Err("record too short".to_string());
    }

    let mut offset = 0;

    let txn_id = u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let txn_epoch = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let record_epoch = u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let page_id = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let begin_seq = u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let has_end_seq = buf[offset];
    offset += 1;
    let end_seq = if has_end_seq == 1 {
        if offset + 8 > buf.len() {
            return Err("end_seq truncated".to_string());
        }
        let seq = u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap());
        offset += 8;
        Some(CommitSeq::new(seq))
    } else {
        None
    };
    if offset + 4 > buf.len() {
        return Err("before_image length truncated".to_string());
    }
    let before_len = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    if offset + before_len > buf.len() {
        return Err("before_image truncated".to_string());
    }
    let before_image = buf[offset..offset + before_len].to_vec();
    offset += before_len;
    if offset + 4 > buf.len() {
        return Err("after_image length truncated".to_string());
    }
    let after_len = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    if offset + after_len > buf.len() {
        return Err("after_image truncated".to_string());
    }
    let after_image = buf[offset..offset + after_len].to_vec();

    let txn_id = fsqlite_types::TxnId::new(txn_id).ok_or("invalid txn_id (zero)")?;
    let page_id = PageNumber::new(page_id).ok_or("invalid page_id (zero)")?;

    Ok(WalRecord {
        txn_token: TxnToken::new(txn_id, fsqlite_types::TxnEpoch::new(txn_epoch)),
        epoch: record_epoch,
        page_id,
        begin_seq: CommitSeq::new(begin_seq),
        end_seq,
        before_image,
        after_image,
    })
}

// ---------------------------------------------------------------------------
// WAL Frame for Parallel Submission
// ---------------------------------------------------------------------------

/// A WAL frame submitted for parallel writing.
#[derive(Debug, Clone)]
pub struct ParallelWalFrame {
    /// Page number.
    pub page_number: PageNumber,
    /// Page data (owned copy for buffering).
    pub page_data: Vec<u8>,
    /// Database size in pages for commit frames, or 0 for non-commit frames.
    pub db_size_if_commit: u32,
}

/// A batch of WAL frames from a single transaction.
#[derive(Debug, Clone)]
pub struct ParallelWalBatch {
    /// Transaction token identifying this batch.
    pub txn_token: TxnToken,
    /// Commit sequence assigned to this batch.
    pub commit_seq: CommitSeq,
    /// Frames in write order.
    pub frames: Vec<ParallelWalFrame>,
}

impl ParallelWalBatch {
    /// Create a new batch from the given frames.
    #[must_use]
    pub fn new(txn_token: TxnToken, commit_seq: CommitSeq, frames: Vec<ParallelWalFrame>) -> Self {
        Self {
            txn_token,
            commit_seq,
            frames,
        }
    }
}

// ---------------------------------------------------------------------------
// Parallel WAL Coordinator
// ---------------------------------------------------------------------------

/// Per-database parallel WAL coordinator.
///
/// This coordinator manages per-thread WAL buffers and epoch-based flushing.
/// It replaces the global WAL append mutex with lock-free per-thread appends.
pub struct ParallelWalCoordinator {
    /// The epoch-based buffer coordinator (Arc for ticker thread sharing).
    inner: Arc<EpochOrderCoordinator>,
    /// Path to the database (for segment file naming).
    db_path: PathBuf,
    /// Configuration.
    config: ParallelWalConfig,
    /// Whether the coordinator is running (Arc for ticker thread sharing).
    running: Arc<AtomicBool>,
    /// Epoch batches drained from memory but not yet durably written.
    pending_batches: Arc<Mutex<VecDeque<EpochFlushBatch>>>,
    /// Epoch ticker handle (spawned on start).
    ticker_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for ParallelWalCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelWalCoordinator")
            .field("db_path", &self.db_path)
            .field("config", &self.config)
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ParallelWalCoordinator {
    /// Create a new parallel WAL coordinator for the given database path.
    #[must_use]
    pub fn new(db_path: &Path, config: ParallelWalConfig) -> Self {
        let buffer_config = BufferConfig {
            capacity_bytes: config.buffer_capacity_bytes,
            ..BufferConfig::default()
        };
        let epoch_config = EpochConfig {
            advance_interval_ms: config.epoch_interval_ms,
        };

        Self {
            inner: Arc::new(EpochOrderCoordinator::new(
                config.slot_count,
                buffer_config,
                epoch_config,
            )),
            db_path: db_path.to_path_buf(),
            config,
            running: Arc::new(AtomicBool::new(false)),
            pending_batches: Arc::new(Mutex::new(VecDeque::new())),
            ticker_handle: Mutex::new(None),
        }
    }

    /// Get the current epoch.
    #[must_use]
    pub fn current_epoch(&self) -> u64 {
        self.inner.current_epoch()
    }

    /// Get the durable epoch (all epochs <= this are guaranteed durable).
    #[must_use]
    pub fn durable_epoch(&self) -> Option<u64> {
        self.inner.durable_epoch()
    }

    /// Get the buffer slot index for the current thread.
    #[must_use]
    pub fn thread_slot(&self) -> usize {
        thread_buffer_slot(self.config.slot_count)
    }

    /// Submit a WAL frame batch for the current thread.
    ///
    /// This method appends the batch's frames to the current thread's buffer
    /// with NO global lock. The batch will be flushed when the epoch advances.
    ///
    /// Returns the epoch in which the batch was submitted.
    pub fn submit_batch(&self, batch: ParallelWalBatch) -> Result<u64, String> {
        let slot = self.thread_slot();
        let epoch = self.inner.current_append_epoch();
        let records = batch
            .frames
            .into_iter()
            .map(|frame| WalRecord {
                txn_token: batch.txn_token,
                epoch,
                page_id: frame.page_number,
                begin_seq: batch.commit_seq,
                end_seq: Some(batch.commit_seq),
                before_image: Vec::new(), // WAL frames don't have before images
                after_image: frame.page_data,
            })
            .collect();

        let outcome = self.inner.append_records_to_core(slot, records)?;
        if matches!(outcome, AppendOutcome::Blocked) {
            return Err("buffer blocked, fallback to serialized path".to_string());
        }

        Ok(epoch)
    }

    /// Wait until the given epoch is durable.
    ///
    /// This method blocks until all frames submitted in or before `epoch`
    /// have been flushed to disk.
    pub fn wait_for_epoch_durable(&self, epoch: u64, timeout: Duration) -> Result<(), String> {
        self.inner.wait_until_epoch_durable(epoch, timeout)
    }

    /// Start the background epoch ticker thread.
    ///
    /// The ticker thread advances the epoch at the configured interval (default 10ms),
    /// sealing and flushing all per-thread buffers. This implements the Silo/Aether
    /// group commit pattern where transactions wait for their epoch to become durable.
    pub fn start(&self) -> Result<(), String> {
        self.start_with_fsync(FsyncPolicy::default())
    }

    /// Start the background epoch ticker thread with a specific fsync policy.
    pub fn start_with_fsync(&self, fsync_policy: FsyncPolicy) -> Result<(), String> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err("coordinator already running".to_string());
        }

        // Clone Arc handles for the ticker thread.
        let running = Arc::clone(&self.running);
        let inner = Arc::clone(&self.inner);
        let db_path = self.db_path.clone();
        let pending_batches = Arc::clone(&self.pending_batches);
        let interval = Duration::from_millis(self.config.epoch_interval_ms);
        let flush_timeout = Duration::from_millis(self.config.epoch_interval_ms * 10);

        let handle = std::thread::Builder::new()
            .name("wal-epoch-ticker".to_string())
            .spawn(move || {
                epoch_ticker_loop(
                    running,
                    inner,
                    db_path,
                    pending_batches,
                    interval,
                    flush_timeout,
                    fsync_policy,
                );
            })
            .map_err(|e| format!("failed to spawn epoch ticker thread: {e}"))?;

        let mut ticker_handle = self
            .ticker_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *ticker_handle = Some(handle);

        Ok(())
    }

    /// Stop the background epoch ticker thread.
    ///
    /// Signals the ticker to stop and waits for it to complete its current
    /// flush cycle before returning.
    pub fn stop(&self) {
        // Signal the ticker to stop.
        self.running.store(false, Ordering::Release);

        // Join the ticker thread if running.
        let mut handle = self
            .ticker_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(h) = handle.take() {
            let _ = h.join();
        }
    }

    /// Check if the background epoch ticker is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Manually advance the epoch and flush all buffers.
    ///
    /// This is used for testing or when no background ticker is running.
    pub fn advance_and_flush(&self, timeout: Duration) -> Result<u64, String> {
        flush_pending_batches(
            &self.pending_batches,
            &self.inner,
            &self.db_path,
            FsyncPolicy::default(),
        )?;

        // Slot-level buffer locks serialize a batch append against sealing, so
        // the top-level coordinator can advance without waiting on inactive slots.
        let new_epoch = self.inner.advance_epoch_and_wait(&[], timeout)?;

        let prev_epoch = new_epoch.saturating_sub(1);
        let batch = self.inner.flush_epoch(prev_epoch)?;
        if batch.records.is_empty() {
            self.inner.mark_epoch_durable(prev_epoch);
        } else {
            enqueue_flush_batch(&self.pending_batches, batch);
            flush_pending_batches(
                &self.pending_batches,
                &self.inner,
                &self.db_path,
                FsyncPolicy::default(),
            )?;
        }

        Ok(new_epoch)
    }
}

impl Drop for ParallelWalCoordinator {
    fn drop(&mut self) {
        self.stop();
    }
}

fn enqueue_flush_batch(
    pending_batches: &Arc<Mutex<VecDeque<EpochFlushBatch>>>,
    batch: EpochFlushBatch,
) {
    let mut pending = pending_batches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pending.push_back(batch);
}

fn flush_pending_batches(
    pending_batches: &Arc<Mutex<VecDeque<EpochFlushBatch>>>,
    inner: &EpochOrderCoordinator,
    db_path: &Path,
    fsync_policy: FsyncPolicy,
) -> Result<(), String> {
    loop {
        let next_batch = {
            let mut pending = pending_batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.pop_front()
        };

        let Some(batch) = next_batch else {
            return Ok(());
        };

        if let Err(error) = write_segment(db_path, &batch, fsync_policy) {
            let epoch = batch.epoch;
            let mut pending = pending_batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.push_front(batch);
            return Err(format!("write_segment({epoch}) failed: {error}"));
        }

        inner.mark_epoch_durable(batch.epoch);
    }
}

// ---------------------------------------------------------------------------
// Epoch Ticker Loop
// ---------------------------------------------------------------------------

/// Background thread loop that advances epochs and flushes WAL buffers.
///
/// This implements an epoch-based group commit pattern:
/// 1. Sleep for the configured interval (default 10ms).
/// 2. Advance the global epoch.
/// 3. Flush any prior pending segment writes.
/// 4. Seal and drain the previous epoch's buffers.
/// 5. Write the batch to a segment file.
/// 6. Mark the epoch as durable.
///
/// The loop exits when `running` is set to false.
fn epoch_ticker_loop(
    running: Arc<AtomicBool>,
    inner: Arc<EpochOrderCoordinator>,
    db_path: PathBuf,
    pending_batches: Arc<Mutex<VecDeque<EpochFlushBatch>>>,
    interval: Duration,
    flush_timeout: Duration,
    fsync_policy: FsyncPolicy,
) {
    while running.load(Ordering::Acquire) {
        // Sleep for the epoch interval.
        std::thread::sleep(interval);

        // Check if we should stop before doing work.
        if !running.load(Ordering::Acquire) {
            break;
        }

        if let Err(error) = flush_pending_batches(&pending_batches, &inner, &db_path, fsync_policy)
        {
            eprintln!("epoch ticker: {error}");
            continue;
        }

        // Slot-level buffer locking makes batch submission atomic relative to sealing,
        // so we can advance without stalling on globally inactive slots.
        match inner.advance_epoch_and_wait(&[], flush_timeout) {
            Ok(new_epoch) => {
                let prev_epoch = new_epoch.saturating_sub(1);
                match inner.flush_epoch(prev_epoch) {
                    Ok(batch) => {
                        if batch.records.is_empty() {
                            inner.mark_epoch_durable(prev_epoch);
                        } else {
                            enqueue_flush_batch(&pending_batches, batch);
                            if let Err(error) = flush_pending_batches(
                                &pending_batches,
                                &inner,
                                &db_path,
                                fsync_policy,
                            ) {
                                eprintln!("epoch ticker: {error}");
                            }
                        }
                    }
                    Err(error) => {
                        eprintln!("epoch ticker: flush_epoch({prev_epoch}) failed: {error}");
                    }
                }
            }
            Err(error) => {
                eprintln!("epoch ticker: advance_epoch_and_wait failed: {error}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Global Coordinators Registry
// ---------------------------------------------------------------------------

type CoordinatorRef = Arc<ParallelWalCoordinator>;

static PARALLEL_WAL_COORDINATORS: OnceLock<Mutex<HashMap<PathBuf, CoordinatorRef>>> =
    OnceLock::new();

/// Get or create a parallel WAL coordinator for the given database path.
pub fn parallel_wal_coordinator_for_path(db_path: &Path) -> CoordinatorRef {
    let coordinators = PARALLEL_WAL_COORDINATORS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut coordinators = coordinators
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    Arc::clone(
        coordinators
            .entry(db_path.to_path_buf())
            .or_insert_with(|| {
                Arc::new(ParallelWalCoordinator::new(
                    db_path,
                    ParallelWalConfig::default(),
                ))
            }),
    )
}

/// Remove a parallel WAL coordinator for the given database path.
pub fn remove_parallel_wal_coordinator(db_path: &Path) {
    if let Some(coordinators) = PARALLEL_WAL_COORDINATORS.get() {
        let mut coordinators = coordinators
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(coordinator) = coordinators.remove(db_path) {
            coordinator.stop();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_batch(txn_id: u64, commit_seq: u64) -> ParallelWalBatch {
        ParallelWalBatch::new(
            TxnToken::new(
                fsqlite_types::TxnId::new(txn_id).expect("txn id should be non-zero"),
                fsqlite_types::TxnEpoch::new(0),
            ),
            CommitSeq::new(commit_seq),
            vec![
                ParallelWalFrame {
                    page_number: PageNumber::new(7).expect("page should be non-zero"),
                    page_data: vec![0xAA; 16],
                    db_size_if_commit: 0,
                },
                ParallelWalFrame {
                    page_number: PageNumber::new(9).expect("page should be non-zero"),
                    page_data: vec![0xBB; 24],
                    db_size_if_commit: 12,
                },
            ],
        )
    }

    #[test]
    fn test_parallel_wal_coordinator_creation() {
        let path = PathBuf::from("/tmp/test.db");
        let coordinator = ParallelWalCoordinator::new(&path, ParallelWalConfig::default());

        assert_eq!(coordinator.current_epoch(), 0);
        assert_eq!(coordinator.durable_epoch(), None);
    }

    #[test]
    fn test_thread_slot_assignment() {
        let path = PathBuf::from("/tmp/test.db");
        let config = ParallelWalConfig {
            slot_count: 4,
            ..ParallelWalConfig::default()
        };
        let coordinator = ParallelWalCoordinator::new(&path, config);

        // Thread slot should be consistent for the same thread.
        let slot1 = coordinator.thread_slot();
        let slot2 = coordinator.thread_slot();
        assert_eq!(slot1, slot2);
        assert!(slot1 < 4);
    }

    #[test]
    fn test_global_coordinator_registry() {
        let path = PathBuf::from("/tmp/test_registry.db");
        let coord1 = parallel_wal_coordinator_for_path(&path);
        let coord2 = parallel_wal_coordinator_for_path(&path);

        // Should return the same coordinator.
        assert!(Arc::ptr_eq(&coord1, &coord2));

        // Cleanup.
        remove_parallel_wal_coordinator(&path);
    }

    #[test]
    fn test_epoch_ticker_start_stop() {
        let path = PathBuf::from("/tmp/test_ticker.db");
        let config = ParallelWalConfig {
            slot_count: 4,
            epoch_interval_ms: 5, // Fast interval for testing
            ..ParallelWalConfig::default()
        };
        let coordinator = ParallelWalCoordinator::new(&path, config);

        // Initially not running.
        assert!(!coordinator.is_running());

        // Start the ticker.
        coordinator.start().expect("start should succeed");
        assert!(coordinator.is_running());

        // Starting again should fail.
        assert!(coordinator.start().is_err());

        // Let the ticker run for a few epochs.
        std::thread::sleep(Duration::from_millis(25));

        // Epoch should be accessible (exact count depends on timing).
        let _epoch = coordinator.current_epoch();

        // Stop the ticker.
        coordinator.stop();
        assert!(!coordinator.is_running());

        // Stopping again should be a no-op (idempotent).
        coordinator.stop();
        assert!(!coordinator.is_running());
    }

    #[test]
    fn test_epoch_ticker_advances_epochs() {
        let path = PathBuf::from("/tmp/test_ticker_advance.db");
        let config = ParallelWalConfig {
            slot_count: 2,        // Small slot count for testing
            epoch_interval_ms: 5, // Fast interval for testing
            ..ParallelWalConfig::default()
        };
        let coordinator = ParallelWalCoordinator::new(&path, config);

        let initial_epoch = coordinator.current_epoch();

        // Start the ticker and wait for several epochs.
        coordinator.start().expect("start should succeed");
        std::thread::sleep(Duration::from_millis(50));
        coordinator.stop();

        let final_epoch = coordinator.current_epoch();

        assert!(
            final_epoch > initial_epoch,
            "epoch ticker should advance without stalling on inactive slots: initial={initial_epoch}, final={final_epoch}"
        );
    }

    #[test]
    fn test_submit_batch_persists_actual_frame_payloads() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("submit_batch.db");
        let config = ParallelWalConfig {
            slot_count: 1,
            ..ParallelWalConfig::default()
        };
        let coordinator = ParallelWalCoordinator::new(&db_path, config);

        let epoch = coordinator
            .submit_batch(sample_batch(11, 77))
            .expect("submit should succeed");
        assert_eq!(epoch, 0);

        coordinator
            .advance_and_flush(Duration::from_millis(50))
            .expect("flush should succeed");
        assert_eq!(coordinator.durable_epoch(), Some(0));

        let seg_path = segment_path(&db_path, 0);
        let (_, records) = read_segment(&seg_path).expect("segment should read back");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].txn_token.id.get(), 11);
        assert_eq!(records[0].begin_seq, CommitSeq::new(77));
        assert_eq!(records[0].page_id.get(), 7);
        assert_eq!(records[0].after_image, vec![0xAA; 16]);
        assert_eq!(records[1].page_id.get(), 9);
        assert_eq!(records[1].after_image, vec![0xBB; 24]);
    }

    #[test]
    fn test_advance_and_flush_does_not_mark_epoch_durable_on_segment_write_failure() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("missing").join("write_failure.db");
        let config = ParallelWalConfig {
            slot_count: 1,
            ..ParallelWalConfig::default()
        };
        let coordinator = ParallelWalCoordinator::new(&db_path, config);

        coordinator
            .submit_batch(sample_batch(21, 99))
            .expect("submit should succeed");

        let error = coordinator
            .advance_and_flush(Duration::from_millis(50))
            .expect_err("flush should fail when the segment directory is missing");
        assert!(
            error.contains("write_segment(0) failed"),
            "error should preserve the failing epoch: {error}"
        );
        assert_eq!(
            coordinator.durable_epoch(),
            None,
            "failed segment writes must not be reported as durable"
        );
        assert!(
            coordinator
                .wait_for_epoch_durable(0, Duration::from_millis(10))
                .is_err(),
            "durability wait must keep blocking after a failed segment write"
        );
    }

    // -------------------------------------------------------------------------
    // Segment File I/O Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_segment_header_roundtrip() {
        let header = SegmentHeader::new(42, 100);
        let bytes = header.to_bytes();
        let parsed = SegmentHeader::from_bytes(&bytes).expect("should parse");
        assert_eq!(parsed.epoch, 42);
        assert_eq!(parsed.record_count, 100);
    }

    #[test]
    fn test_segment_header_invalid_magic() {
        let mut bytes = [0u8; SEGMENT_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let result = SegmentHeader::from_bytes(&bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid segment magic"));
    }

    #[test]
    fn test_segment_header_checksum_mismatch() {
        let header = SegmentHeader::new(42, 100);
        let mut bytes = header.to_bytes();
        // Corrupt the epoch field
        bytes[8] ^= 0xFF;
        let result = SegmentHeader::from_bytes(&bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("checksum mismatch"));
    }

    #[test]
    fn test_segment_path_generation() {
        let db_path = PathBuf::from("/tmp/mydb.sqlite");
        let path = segment_path(&db_path, 0x1234_5678_9ABC_DEF0);
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "mydb.sqlite-wal-seg-123456789abcdef0"
        );
    }

    #[test]
    fn test_segment_write_and_read() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("test.db");

        // Create a batch with some records
        let records = vec![
            WalRecord {
                txn_token: TxnToken::new(
                    fsqlite_types::TxnId::new(1).unwrap(),
                    fsqlite_types::TxnEpoch::new(0),
                ),
                epoch: 5,
                page_id: PageNumber::new(1).unwrap(),
                begin_seq: CommitSeq::new(100),
                end_seq: Some(CommitSeq::new(100)),
                before_image: vec![0u8; 32],
                after_image: vec![1u8; 32],
            },
            WalRecord {
                txn_token: TxnToken::new(
                    fsqlite_types::TxnId::new(2).unwrap(),
                    fsqlite_types::TxnEpoch::new(1),
                ),
                epoch: 5,
                page_id: PageNumber::new(2).unwrap(),
                begin_seq: CommitSeq::new(101),
                end_seq: None,
                before_image: Vec::new(),
                after_image: vec![2u8; 64],
            },
        ];

        let batch = EpochFlushBatch {
            epoch: 5,
            records,
            records_per_core: vec![1, 1],
        };

        // Write the segment
        let bytes_written =
            write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");
        assert!(bytes_written > SEGMENT_HEADER_SIZE);

        // Read it back
        let seg_path = segment_path(&db_path, 5);
        let (header, records) = read_segment(&seg_path).expect("read should succeed");

        assert_eq!(header.epoch, 5);
        assert_eq!(header.record_count, 2);
        assert_eq!(records.len(), 2);

        // Verify first record
        assert_eq!(records[0].txn_token.id.get(), 1);
        assert_eq!(records[0].page_id.get(), 1);
        assert_eq!(records[0].before_image.len(), 32);
        assert_eq!(records[0].after_image.len(), 32);
        assert_eq!(records[0].end_seq, Some(CommitSeq::new(100)));

        // Verify second record
        assert_eq!(records[1].txn_token.id.get(), 2);
        assert_eq!(records[1].page_id.get(), 2);
        assert_eq!(records[1].before_image.len(), 0);
        assert_eq!(records[1].after_image.len(), 64);
        assert_eq!(records[1].end_seq, None);

        // Cleanup
        delete_segment(&seg_path).expect("delete should succeed");
    }

    #[test]
    fn test_segment_write_and_recovery_canonicalize_intra_epoch_order() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("ordered.db");
        let page_id = PageNumber::new(1).unwrap();

        let later = WalRecord {
            txn_token: TxnToken::new(
                fsqlite_types::TxnId::new(2).unwrap(),
                fsqlite_types::TxnEpoch::new(0),
            ),
            epoch: 7,
            page_id,
            begin_seq: CommitSeq::new(200),
            end_seq: Some(CommitSeq::new(200)),
            before_image: Vec::new(),
            after_image: vec![0x22; 8],
        };
        let earlier = WalRecord {
            txn_token: TxnToken::new(
                fsqlite_types::TxnId::new(1).unwrap(),
                fsqlite_types::TxnEpoch::new(0),
            ),
            epoch: 7,
            page_id,
            begin_seq: CommitSeq::new(100),
            end_seq: Some(CommitSeq::new(100)),
            before_image: Vec::new(),
            after_image: vec![0x11; 8],
        };
        let batch = EpochFlushBatch {
            epoch: 7,
            records: vec![later, earlier],
            records_per_core: vec![1, 1],
        };

        write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");

        let seg_path = segment_path(&db_path, 7);
        let (_, records) = read_segment(&seg_path).expect("read should succeed");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].begin_seq, CommitSeq::new(100));
        assert_eq!(records[1].begin_seq, CommitSeq::new(200));

        let mut page_contents = HashMap::new();
        recover_and_apply_segments(
            &db_path,
            &mut page_contents,
            SegmentRecoveryOptions::default(),
        )
        .expect("recovery should succeed");
        assert_eq!(
            page_contents.get(&page_id.get()),
            Some(&vec![0x22; 8]),
            "recovery must replay the later commit last even if the flushed batch arrived out of order"
        );
    }

    #[test]
    fn test_write_segment_rejects_record_epoch_mismatch() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("mismatch.db");

        let batch = EpochFlushBatch {
            epoch: 5,
            records: vec![WalRecord {
                txn_token: TxnToken::new(
                    fsqlite_types::TxnId::new(1).unwrap(),
                    fsqlite_types::TxnEpoch::new(0),
                ),
                epoch: 4,
                page_id: PageNumber::new(1).unwrap(),
                begin_seq: CommitSeq::new(100),
                end_seq: Some(CommitSeq::new(100)),
                before_image: Vec::new(),
                after_image: vec![0xAB; 8],
            }],
            records_per_core: vec![1],
        };

        let error = write_segment(&db_path, &batch, FsyncPolicy::Off)
            .expect_err("segment write must reject mixed-epoch records");
        assert!(
            error
                .to_string()
                .contains("segment epoch 5 contains record from epoch 4"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_list_segments() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("test.db");

        // Create a few empty segment files
        for epoch in [1u64, 5, 10, 2] {
            let batch = EpochFlushBatch {
                epoch,
                records: Vec::new(),
                records_per_core: Vec::new(),
            };
            write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");
        }

        // List segments
        let segments = list_segments(&db_path).expect("list should succeed");
        assert_eq!(segments.len(), 4);

        // Should be sorted by epoch
        assert_eq!(segments[0].0, 1);
        assert_eq!(segments[1].0, 2);
        assert_eq!(segments[2].0, 5);
        assert_eq!(segments[3].0, 10);

        // Cleanup
        for (_, path) in segments {
            delete_segment(&path).expect("delete should succeed");
        }
    }

    // -------------------------------------------------------------------------
    // Segment Recovery Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_recover_segments_basic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("test.db");

        // Create segments for epochs 1, 2, 3
        for epoch in 1..=3u64 {
            let records = vec![WalRecord {
                txn_token: TxnToken::new(
                    fsqlite_types::TxnId::new(epoch).unwrap(),
                    fsqlite_types::TxnEpoch::new(0),
                ),
                epoch,
                page_id: PageNumber::new(epoch as u32).unwrap(),
                begin_seq: CommitSeq::new(epoch * 100),
                end_seq: Some(CommitSeq::new(epoch * 100)),
                before_image: Vec::new(),
                after_image: vec![epoch as u8; 32],
            }];
            let batch = EpochFlushBatch {
                epoch,
                records,
                records_per_core: vec![1],
            };
            write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");
        }

        // Recover segments
        let options = SegmentRecoveryOptions::default();
        let (result, records) =
            recover_segments(&db_path, options).expect("recovery should succeed");

        assert_eq!(result.segments_recovered, 3);
        assert_eq!(result.records_applied, 3);
        assert_eq!(result.epochs, vec![1, 2, 3]);
        assert!(result.partial_segments.is_empty());

        // Verify records are in epoch order
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].epoch, 1);
        assert_eq!(records[1].epoch, 2);
        assert_eq!(records[2].epoch, 3);

        // Cleanup
        cleanup_segments(&db_path).expect("cleanup should succeed");
    }

    #[test]
    fn test_recover_segments_rejects_header_filename_epoch_mismatch() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("rename.db");
        let batch = EpochFlushBatch {
            epoch: 5,
            records: vec![WalRecord {
                txn_token: TxnToken::new(
                    fsqlite_types::TxnId::new(1).unwrap(),
                    fsqlite_types::TxnEpoch::new(0),
                ),
                epoch: 5,
                page_id: PageNumber::new(1).unwrap(),
                begin_seq: CommitSeq::new(100),
                end_seq: Some(CommitSeq::new(100)),
                before_image: Vec::new(),
                after_image: vec![0xAA; 8],
            }],
            records_per_core: vec![1],
        };
        write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");

        let original = segment_path(&db_path, 5);
        let renamed = segment_path(&db_path, 3);
        std::fs::rename(&original, &renamed).expect("rename should succeed");

        let error = recover_segments(&db_path, SegmentRecoveryOptions::default())
            .expect_err("recovery must fail closed on mismatched epoch metadata");
        assert!(
            error.to_string().contains("mismatched epoch"),
            "unexpected error: {error}"
        );

        let (result, records) = recover_segments(
            &db_path,
            SegmentRecoveryOptions {
                skip_corrupt: true,
                ..Default::default()
            },
        )
        .expect("skip_corrupt should ignore the bad segment");
        assert_eq!(result.segments_recovered, 0);
        assert_eq!(result.partial_segments, vec![renamed]);
        assert!(records.is_empty());
    }

    #[test]
    fn test_recover_and_apply_segments_skip_corrupt_stops_at_first_bad_epoch() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("prefix.db");

        for epoch in 1..=3u64 {
            let batch = EpochFlushBatch {
                epoch,
                records: vec![WalRecord {
                    txn_token: TxnToken::new(
                        fsqlite_types::TxnId::new(epoch).unwrap(),
                        fsqlite_types::TxnEpoch::new(0),
                    ),
                    epoch,
                    page_id: PageNumber::new(1).unwrap(),
                    begin_seq: CommitSeq::new(epoch * 100),
                    end_seq: Some(CommitSeq::new(epoch * 100)),
                    before_image: Vec::new(),
                    after_image: vec![epoch as u8; 16],
                }],
                records_per_core: vec![1],
            };
            write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");
        }

        let corrupt_epoch_path = segment_path(&db_path, 2);
        std::fs::write(&corrupt_epoch_path, [0xFF_u8; 8]).expect("corrupt write should succeed");

        let mut page_contents = HashMap::new();
        let result = recover_and_apply_segments(
            &db_path,
            &mut page_contents,
            SegmentRecoveryOptions {
                skip_corrupt: true,
                ..Default::default()
            },
        )
        .expect("skip_corrupt should return the durable prefix");

        assert_eq!(result.segments_recovered, 1);
        assert_eq!(result.records_applied, 1);
        assert_eq!(result.epochs, vec![1]);
        assert_eq!(
            result.partial_segments,
            vec![segment_path(&db_path, 2), segment_path(&db_path, 3)]
        );

        let page = page_contents
            .get(&1)
            .expect("prefix recovery should apply the last durable epoch only");
        assert!(
            page.iter().all(|&byte| byte == 1),
            "recovery must stop before epoch 3 once epoch 2 is corrupt"
        );
    }

    #[test]
    fn test_recover_and_apply_segments() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("test.db");

        // Create segments that update the same page multiple times
        let page_id = 1u32;
        for epoch in 1..=3u64 {
            let records = vec![WalRecord {
                txn_token: TxnToken::new(
                    fsqlite_types::TxnId::new(epoch).unwrap(),
                    fsqlite_types::TxnEpoch::new(0),
                ),
                epoch,
                page_id: PageNumber::new(page_id).unwrap(),
                begin_seq: CommitSeq::new(epoch * 100),
                end_seq: Some(CommitSeq::new(epoch * 100)),
                before_image: Vec::new(),
                after_image: vec![epoch as u8; 32], // Different content each epoch
            }];
            let batch = EpochFlushBatch {
                epoch,
                records,
                records_per_core: vec![1],
            };
            write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");
        }

        // Recover and apply to page cache
        let mut page_contents = HashMap::new();
        let options = SegmentRecoveryOptions {
            delete_after_recovery: true,
            ..Default::default()
        };
        let result = recover_and_apply_segments(&db_path, &mut page_contents, options)
            .expect("should succeed");

        assert_eq!(result.segments_recovered, 3);

        // Page should have the final epoch's contents (epoch 3)
        let page = page_contents.get(&page_id).expect("page should exist");
        assert_eq!(page.len(), 32);
        assert!(page.iter().all(|&b| b == 3), "should have epoch 3 content");

        // Segments should be deleted
        let remaining = list_segments(&db_path).expect("list should succeed");
        assert!(
            remaining.is_empty(),
            "segments should be deleted after recovery"
        );
    }

    #[test]
    fn test_max_durable_epoch() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("test.db");

        // Initially no segments
        let max = max_durable_epoch(&db_path).expect("should succeed");
        assert_eq!(max, None);

        // Create segments
        for epoch in [5u64, 10, 3] {
            let batch = EpochFlushBatch {
                epoch,
                records: Vec::new(),
                records_per_core: Vec::new(),
            };
            write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");
        }

        // Max should be 10
        let max = max_durable_epoch(&db_path).expect("should succeed");
        assert_eq!(max, Some(10));

        // Cleanup
        cleanup_segments(&db_path).expect("cleanup should succeed");

        // Now max should be None again
        let max = max_durable_epoch(&db_path).expect("should succeed");
        assert_eq!(max, None);
    }

    #[test]
    fn test_cleanup_segments() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("test.db");

        // Create segments
        for epoch in 1..=5u64 {
            let batch = EpochFlushBatch {
                epoch,
                records: Vec::new(),
                records_per_core: Vec::new(),
            };
            write_segment(&db_path, &batch, FsyncPolicy::Off).expect("write should succeed");
        }

        // Verify segments exist
        let segments = list_segments(&db_path).expect("list should succeed");
        assert_eq!(segments.len(), 5);

        // Cleanup
        let count = cleanup_segments(&db_path).expect("cleanup should succeed");
        assert_eq!(count, 5);

        // Verify segments are gone
        let segments = list_segments(&db_path).expect("list should succeed");
        assert!(segments.is_empty());
    }
}
