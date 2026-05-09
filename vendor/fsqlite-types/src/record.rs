//! SQLite record format serialization and deserialization.
//!
//! A SQLite record consists of a header followed by data. The header contains
//! the size of the header itself (as a varint) followed by serial type codes
//! (each as a varint) for every column. The data section contains the column
//! values packed sequentially according to their serial types.
//!
//! See: <https://www.sqlite.org/fileformat.html#record_format>

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::time::Instant;

use crate::serial_type::{
    SerialTypeClass, classify_serial_type, read_varint, serial_type_for_blob,
    serial_type_for_integer, serial_type_for_text, serial_type_len, varint_len, write_varint,
};
use crate::value::SqliteValue;

static FSQLITE_RECORD_PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);
static FSQLITE_RECORD_PARSE_CALLS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_PARSE_INTO_CALLS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_PARSE_COLUMN_CALLS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_BYTES_SCANNED: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_VEC_CAPACITY_SLOTS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_DECODE_TIME_NS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_VALUE_NULLS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_VALUE_INTEGERS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_VALUE_FLOATS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_VALUE_TEXTS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_VALUE_BLOBS: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_TEXT_BYTES: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_BLOB_BYTES: AtomicU64 = AtomicU64::new(0);
static FSQLITE_RECORD_PARSE_CALLS_BY_SCOPE: [AtomicU64; RECORD_PROFILE_SCOPE_COUNT] =
    [const { AtomicU64::new(0) }; RECORD_PROFILE_SCOPE_COUNT];
static FSQLITE_RECORD_PARSE_INTO_CALLS_BY_SCOPE: [AtomicU64; RECORD_PROFILE_SCOPE_COUNT] =
    [const { AtomicU64::new(0) }; RECORD_PROFILE_SCOPE_COUNT];
static FSQLITE_RECORD_PARSE_COLUMN_CALLS_BY_SCOPE: [AtomicU64; RECORD_PROFILE_SCOPE_COUNT] =
    [const { AtomicU64::new(0) }; RECORD_PROFILE_SCOPE_COUNT];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum RecordProfileScope {
    Unattributed = 0,
    CoreConnection = 1,
    CoreCompatPersist = 2,
    VdbeEngine = 3,
    VdbeVectorizedScan = 4,
    BtreeCursor = 5,
}

impl RecordProfileScope {
    #[must_use]
    const fn as_index(self) -> usize {
        self as usize
    }
}

const RECORD_PROFILE_SCOPE_COUNT: usize = RecordProfileScope::BtreeCursor as usize + 1;

thread_local! {
    static CURRENT_RECORD_PROFILE_SCOPE: Cell<RecordProfileScope> =
        const { Cell::new(RecordProfileScope::Unattributed) };
}

#[derive(Debug)]
pub struct RecordProfileScopeGuard {
    previous: Option<RecordProfileScope>,
}

impl Drop for RecordProfileScopeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            CURRENT_RECORD_PROFILE_SCOPE.with(|current| current.set(previous));
        }
    }
}

#[must_use]
pub fn enter_record_profile_scope(scope: RecordProfileScope) -> RecordProfileScopeGuard {
    if !record_profile_enabled() {
        return RecordProfileScopeGuard { previous: None };
    }
    let previous = CURRENT_RECORD_PROFILE_SCOPE.with(|current| {
        let previous = current.get();
        current.set(scope);
        previous
    });
    RecordProfileScopeGuard {
        previous: Some(previous),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ValueTypeProfileSnapshot {
    pub null_count: u64,
    pub integer_count: u64,
    pub float_count: u64,
    pub text_count: u64,
    pub blob_count: u64,
    pub text_bytes: u64,
    pub blob_bytes: u64,
}

impl ValueTypeProfileSnapshot {
    #[must_use]
    pub fn total_values(self) -> u64 {
        self.null_count
            .saturating_add(self.integer_count)
            .saturating_add(self.float_count)
            .saturating_add(self.text_count)
            .saturating_add(self.blob_count)
    }

    pub fn saturating_add_assign(&mut self, other: Self) {
        self.null_count = self.null_count.saturating_add(other.null_count);
        self.integer_count = self.integer_count.saturating_add(other.integer_count);
        self.float_count = self.float_count.saturating_add(other.float_count);
        self.text_count = self.text_count.saturating_add(other.text_count);
        self.blob_count = self.blob_count.saturating_add(other.blob_count);
        self.text_bytes = self.text_bytes.saturating_add(other.text_bytes);
        self.blob_bytes = self.blob_bytes.saturating_add(other.blob_bytes);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordHotPathProfileSnapshot {
    pub parse_record_calls: u64,
    pub parse_record_into_calls: u64,
    pub parse_record_column_calls: u64,
    pub record_bytes_scanned: u64,
    pub record_vec_capacity_slots: u64,
    pub decode_time_ns: u64,
    pub decoded_values: ValueTypeProfileSnapshot,
    pub callsite_breakdown: RecordProfileScopeBreakdownSnapshot,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordProfileScopeCountersSnapshot {
    pub parse_record_calls: u64,
    pub parse_record_into_calls: u64,
    pub parse_record_column_calls: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordProfileScopeBreakdownSnapshot {
    pub unattributed: RecordProfileScopeCountersSnapshot,
    pub core_connection: RecordProfileScopeCountersSnapshot,
    pub core_compat_persist: RecordProfileScopeCountersSnapshot,
    pub vdbe_engine: RecordProfileScopeCountersSnapshot,
    pub vdbe_vectorized_scan: RecordProfileScopeCountersSnapshot,
    pub btree_cursor: RecordProfileScopeCountersSnapshot,
}

pub fn set_record_profile_enabled(enabled: bool) {
    FSQLITE_RECORD_PROFILE_ENABLED.store(enabled, AtomicOrdering::Relaxed);
}

#[must_use]
pub fn record_profile_enabled() -> bool {
    FSQLITE_RECORD_PROFILE_ENABLED.load(AtomicOrdering::Relaxed)
}

pub fn reset_record_profile() {
    FSQLITE_RECORD_PARSE_CALLS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_PARSE_INTO_CALLS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_PARSE_COLUMN_CALLS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_BYTES_SCANNED.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_VEC_CAPACITY_SLOTS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_DECODE_TIME_NS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_VALUE_NULLS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_VALUE_INTEGERS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_VALUE_FLOATS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_VALUE_TEXTS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_VALUE_BLOBS.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_TEXT_BYTES.store(0, AtomicOrdering::Relaxed);
    FSQLITE_RECORD_BLOB_BYTES.store(0, AtomicOrdering::Relaxed);
    for counter in &FSQLITE_RECORD_PARSE_CALLS_BY_SCOPE {
        counter.store(0, AtomicOrdering::Relaxed);
    }
    for counter in &FSQLITE_RECORD_PARSE_INTO_CALLS_BY_SCOPE {
        counter.store(0, AtomicOrdering::Relaxed);
    }
    for counter in &FSQLITE_RECORD_PARSE_COLUMN_CALLS_BY_SCOPE {
        counter.store(0, AtomicOrdering::Relaxed);
    }
}

#[must_use]
pub fn record_profile_snapshot() -> RecordHotPathProfileSnapshot {
    RecordHotPathProfileSnapshot {
        parse_record_calls: FSQLITE_RECORD_PARSE_CALLS.load(AtomicOrdering::Relaxed),
        parse_record_into_calls: FSQLITE_RECORD_PARSE_INTO_CALLS.load(AtomicOrdering::Relaxed),
        parse_record_column_calls: FSQLITE_RECORD_PARSE_COLUMN_CALLS.load(AtomicOrdering::Relaxed),
        record_bytes_scanned: FSQLITE_RECORD_BYTES_SCANNED.load(AtomicOrdering::Relaxed),
        record_vec_capacity_slots: FSQLITE_RECORD_VEC_CAPACITY_SLOTS.load(AtomicOrdering::Relaxed),
        decode_time_ns: FSQLITE_RECORD_DECODE_TIME_NS.load(AtomicOrdering::Relaxed),
        decoded_values: ValueTypeProfileSnapshot {
            null_count: FSQLITE_RECORD_VALUE_NULLS.load(AtomicOrdering::Relaxed),
            integer_count: FSQLITE_RECORD_VALUE_INTEGERS.load(AtomicOrdering::Relaxed),
            float_count: FSQLITE_RECORD_VALUE_FLOATS.load(AtomicOrdering::Relaxed),
            text_count: FSQLITE_RECORD_VALUE_TEXTS.load(AtomicOrdering::Relaxed),
            blob_count: FSQLITE_RECORD_VALUE_BLOBS.load(AtomicOrdering::Relaxed),
            text_bytes: FSQLITE_RECORD_TEXT_BYTES.load(AtomicOrdering::Relaxed),
            blob_bytes: FSQLITE_RECORD_BLOB_BYTES.load(AtomicOrdering::Relaxed),
        },
        callsite_breakdown: RecordProfileScopeBreakdownSnapshot {
            unattributed: record_profile_scope_snapshot(RecordProfileScope::Unattributed),
            core_connection: record_profile_scope_snapshot(RecordProfileScope::CoreConnection),
            core_compat_persist: record_profile_scope_snapshot(
                RecordProfileScope::CoreCompatPersist,
            ),
            vdbe_engine: record_profile_scope_snapshot(RecordProfileScope::VdbeEngine),
            vdbe_vectorized_scan: record_profile_scope_snapshot(
                RecordProfileScope::VdbeVectorizedScan,
            ),
            btree_cursor: record_profile_scope_snapshot(RecordProfileScope::BtreeCursor),
        },
    }
}

#[derive(Debug, Clone, Copy)]
enum RecordParseKind {
    Full,
    Into,
    Column,
}

#[must_use]
fn current_record_profile_scope() -> RecordProfileScope {
    CURRENT_RECORD_PROFILE_SCOPE.with(Cell::get)
}

fn note_record_profile_parse_call(kind: RecordParseKind) {
    let idx = current_record_profile_scope().as_index();
    match kind {
        RecordParseKind::Full => {
            FSQLITE_RECORD_PARSE_CALLS.fetch_add(1, AtomicOrdering::Relaxed);
            FSQLITE_RECORD_PARSE_CALLS_BY_SCOPE[idx].fetch_add(1, AtomicOrdering::Relaxed);
        }
        RecordParseKind::Into => {
            FSQLITE_RECORD_PARSE_INTO_CALLS.fetch_add(1, AtomicOrdering::Relaxed);
            FSQLITE_RECORD_PARSE_INTO_CALLS_BY_SCOPE[idx].fetch_add(1, AtomicOrdering::Relaxed);
        }
        RecordParseKind::Column => {
            FSQLITE_RECORD_PARSE_COLUMN_CALLS.fetch_add(1, AtomicOrdering::Relaxed);
            FSQLITE_RECORD_PARSE_COLUMN_CALLS_BY_SCOPE[idx].fetch_add(1, AtomicOrdering::Relaxed);
        }
    }
}

#[must_use]
fn record_profile_scope_snapshot(scope: RecordProfileScope) -> RecordProfileScopeCountersSnapshot {
    let idx = scope.as_index();
    RecordProfileScopeCountersSnapshot {
        parse_record_calls: FSQLITE_RECORD_PARSE_CALLS_BY_SCOPE[idx].load(AtomicOrdering::Relaxed),
        parse_record_into_calls: FSQLITE_RECORD_PARSE_INTO_CALLS_BY_SCOPE[idx]
            .load(AtomicOrdering::Relaxed),
        parse_record_column_calls: FSQLITE_RECORD_PARSE_COLUMN_CALLS_BY_SCOPE[idx]
            .load(AtomicOrdering::Relaxed),
    }
}

fn note_decoded_value(value: &SqliteValue) {
    match value {
        SqliteValue::Null => {
            FSQLITE_RECORD_VALUE_NULLS.fetch_add(1, AtomicOrdering::Relaxed);
        }
        SqliteValue::Integer(_) => {
            FSQLITE_RECORD_VALUE_INTEGERS.fetch_add(1, AtomicOrdering::Relaxed);
        }
        SqliteValue::Float(_) => {
            FSQLITE_RECORD_VALUE_FLOATS.fetch_add(1, AtomicOrdering::Relaxed);
        }
        SqliteValue::Text(text) => {
            FSQLITE_RECORD_VALUE_TEXTS.fetch_add(1, AtomicOrdering::Relaxed);
            FSQLITE_RECORD_TEXT_BYTES.fetch_add(
                u64::try_from(text.len()).unwrap_or(u64::MAX),
                AtomicOrdering::Relaxed,
            );
        }
        SqliteValue::Blob(blob) => {
            FSQLITE_RECORD_VALUE_BLOBS.fetch_add(1, AtomicOrdering::Relaxed);
            FSQLITE_RECORD_BLOB_BYTES.fetch_add(
                u64::try_from(blob.len()).unwrap_or(u64::MAX),
                AtomicOrdering::Relaxed,
            );
        }
    }
}

/// Parse a serialized record into a list of `SqliteValue`s.
///
/// The input `data` should be the complete record (header + body).
/// Returns `None` if the record is malformed.
#[allow(clippy::cast_possible_truncation)]
pub fn parse_record(data: &[u8]) -> Option<Vec<SqliteValue>> {
    // A typical record has ~4-8 columns. We can estimate capacity from data len / 8 as a heuristic,
    // clamped between 4 and 64, to avoid reallocation for the majority of rows.
    let cap = (data.len() / 8).clamp(4, 64);
    if record_profile_enabled() {
        note_record_profile_parse_call(RecordParseKind::Full);
        FSQLITE_RECORD_VEC_CAPACITY_SLOTS.fetch_add(
            u64::try_from(cap).unwrap_or(u64::MAX),
            AtomicOrdering::Relaxed,
        );
    }
    let mut values = Vec::with_capacity(cap);
    parse_record_into(data, &mut values)?;
    Some(values)
}

/// Parse a serialized record into an existing `Vec<SqliteValue>`.
///
/// Existing slots are reused when possible so repeated row decodes can keep
/// text/blob backing storage alive across iterations.
#[allow(clippy::cast_possible_truncation)]
pub fn parse_record_into(data: &[u8], values: &mut Vec<SqliteValue>) -> Option<()> {
    let profile_enabled = record_profile_enabled();
    let start = profile_enabled.then(Instant::now);
    if profile_enabled {
        note_record_profile_parse_call(RecordParseKind::Into);
        FSQLITE_RECORD_BYTES_SCANNED.fetch_add(
            u64::try_from(data.len()).unwrap_or(u64::MAX),
            AtomicOrdering::Relaxed,
        );
    }
    if data.is_empty() {
        return None;
    }

    // Read the header size.
    let (header_size_u64, hdr_varint_len) = read_varint(data)?;
    let header_size = usize::try_from(header_size_u64).unwrap_or(usize::MAX);

    if header_size > data.len() || header_size < hdr_varint_len {
        return None;
    }

    // Parse serial types and values in a single pass.
    let mut offset = hdr_varint_len;
    let mut body_offset = header_size;
    let mut decoded_count = 0usize;

    while offset < header_size {
        let (serial_type, consumed) = read_varint(&data[offset..header_size])?;
        offset += consumed;

        let value_len_u64 = serial_type_len(serial_type)?;
        let value_len = usize::try_from(value_len_u64).unwrap_or(usize::MAX);

        let end = body_offset.checked_add(value_len)?;

        if end > data.len() {
            return None;
        }

        let value_bytes = &data[body_offset..end];
        // Reuse caller-owned scratch slots so repeated row decodes keep
        // existing text/blob backing storage alive across iterations.
        if let Some(slot) = values.get_mut(decoded_count) {
            decode_value_into(serial_type, value_bytes, slot, profile_enabled)?;
        } else {
            let value = decode_value(serial_type, value_bytes, profile_enabled)?;
            values.push(value);
        }
        body_offset = end;
        decoded_count += 1;
    }

    if body_offset != data.len() {
        return None;
    }

    values.truncate(decoded_count);

    if let Some(start) = start {
        FSQLITE_RECORD_DECODE_TIME_NS.fetch_add(
            u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            AtomicOrdering::Relaxed,
        );
    }
    Some(())
}

/// Parse a single column from a serialized record, avoiding allocating for other columns.
///
/// Returns `None` if the record is malformed or if `col_idx` is out of bounds.
#[allow(clippy::cast_possible_truncation)]
pub fn parse_record_column(data: &[u8], col_idx: usize) -> Option<SqliteValue> {
    let profile_enabled = record_profile_enabled();
    let start = profile_enabled.then(Instant::now);
    if profile_enabled {
        note_record_profile_parse_call(RecordParseKind::Column);
        FSQLITE_RECORD_BYTES_SCANNED.fetch_add(
            u64::try_from(data.len()).unwrap_or(u64::MAX),
            AtomicOrdering::Relaxed,
        );
    }
    if data.is_empty() {
        return None;
    }

    // Read the header size.
    let (header_size_u64, hdr_varint_len) = read_varint(data)?;
    let header_size = usize::try_from(header_size_u64).unwrap_or(usize::MAX);

    if header_size > data.len() || header_size < hdr_varint_len {
        return None;
    }

    let mut offset = hdr_varint_len;
    let mut body_offset = header_size;
    let mut current_idx = 0;

    while offset < header_size {
        let (serial_type, consumed) = read_varint(&data[offset..header_size])?;
        offset += consumed;

        let value_len_u64 = serial_type_len(serial_type)?;
        let value_len = usize::try_from(value_len_u64).unwrap_or(usize::MAX);
        let end = body_offset.checked_add(value_len)?;

        if end > data.len() {
            return None;
        }

        if current_idx == col_idx {
            let value_bytes = &data[body_offset..end];
            let value = decode_value(serial_type, value_bytes, profile_enabled);
            if let Some(start) = start {
                FSQLITE_RECORD_DECODE_TIME_NS.fetch_add(
                    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    AtomicOrdering::Relaxed,
                );
            }
            return value;
        }

        body_offset = end;
        current_idx += 1;
    }

    None
}

/// Count the number of serialized columns in a SQLite record without decoding
/// the values themselves.
///
/// Returns `None` if the record is malformed.
#[allow(clippy::cast_possible_truncation)]
pub fn record_column_count(data: &[u8]) -> Option<usize> {
    if data.is_empty() {
        return None;
    }

    let (header_size_u64, hdr_varint_len) = read_varint(data)?;
    let header_size = usize::try_from(header_size_u64).unwrap_or(usize::MAX);

    if header_size > data.len() || header_size < hdr_varint_len {
        return None;
    }

    let mut offset = hdr_varint_len;
    let mut count = 0_usize;

    while offset < header_size {
        let (_, consumed) = read_varint(&data[offset..header_size])?;
        offset += consumed;
        count += 1;
    }

    Some(count)
}

// ---------------------------------------------------------------------------
// Lazy column decode — Record Offset Table
// ---------------------------------------------------------------------------

/// Pre-parsed record header offset for a single column.
///
/// Stores the serial type and byte range within the record body so that
/// individual columns can be decoded on demand without scanning the entire
/// record's data section.
#[derive(Debug, Clone, Copy)]
pub struct ColumnOffset {
    /// SQLite serial type code for this column.
    pub serial_type: u64,
    /// Start offset of the value bytes within the record (from byte 0).
    pub body_offset: u32,
    /// Length of the value bytes.
    pub value_len: u32,
}

/// Parse only the record header into a caller-owned offset table.
///
/// This is the lazy-decode counterpart to [`parse_record_into`]:
/// it scans the header to learn each column's serial type and byte range
/// but does **not** decode any column values.  Individual columns can
/// then be decoded on demand via [`decode_value`] with the corresponding
/// [`ColumnOffset`].
///
/// Returns `Some(column_count)` on success, `None` if the header is
/// malformed.
#[allow(clippy::cast_possible_truncation)]
pub fn parse_record_header_into(data: &[u8], offsets: &mut Vec<ColumnOffset>) -> Option<usize> {
    offsets.clear();
    if data.is_empty() {
        return None;
    }

    let (header_size_u64, hdr_varint_len) = read_varint(data)?;
    let header_size = usize::try_from(header_size_u64).unwrap_or(usize::MAX);

    if header_size > data.len() || header_size < hdr_varint_len {
        return None;
    }

    let mut offset = hdr_varint_len;
    let mut body_offset = header_size;

    while offset < header_size {
        let (serial_type, consumed) = read_varint(&data[offset..header_size])?;
        offset += consumed;

        let value_len_u64 = serial_type_len(serial_type)?;
        let value_len = usize::try_from(value_len_u64).unwrap_or(usize::MAX);
        let body_start = body_offset;
        body_offset = body_offset.checked_add(value_len)?;

        if body_offset > data.len() {
            return None;
        }

        offsets.push(ColumnOffset {
            serial_type,
            body_offset: u32::try_from(body_start).ok()?,
            value_len: u32::try_from(value_len).ok()?,
        });
    }

    // Validate that the body section is fully consumed.
    if body_offset != data.len() {
        return None;
    }

    Some(offsets.len())
}

/// Parse only the record header from a partial record prefix.
///
/// Unlike [`parse_record_header_into`], this only requires the full header
/// bytes to be present. The body bytes may be truncated because callers can
/// fetch more payload after learning which column end offset is needed.
#[allow(clippy::cast_possible_truncation)]
pub fn parse_record_header_prefix_into(
    data: &[u8],
    offsets: &mut Vec<ColumnOffset>,
) -> Option<usize> {
    offsets.clear();
    if data.is_empty() {
        return None;
    }

    let (header_size_u64, hdr_varint_len) = read_varint(data)?;
    let header_size = usize::try_from(header_size_u64).unwrap_or(usize::MAX);
    if header_size > data.len() || header_size < hdr_varint_len {
        return None;
    }

    let mut offset = hdr_varint_len;
    let mut body_offset = header_size;

    while offset < header_size {
        let (serial_type, consumed) = read_varint(&data[offset..header_size])?;
        offset += consumed;

        let value_len_u64 = serial_type_len(serial_type)?;
        let value_len = usize::try_from(value_len_u64).unwrap_or(usize::MAX);
        let body_start = body_offset;
        body_offset = body_offset.checked_add(value_len)?;

        offsets.push(ColumnOffset {
            serial_type,
            body_offset: u32::try_from(body_start).ok()?,
            value_len: u32::try_from(value_len).ok()?,
        });
    }

    Some(offsets.len())
}

/// Decode a single column from previously-parsed offset table + raw record data.
///
/// This avoids re-scanning the header on each column access and only
/// materializes the value for the requested column.
#[inline]
pub fn decode_column_from_offset(
    data: &[u8],
    col: &ColumnOffset,
    profile_enabled: bool,
) -> Option<SqliteValue> {
    let start = col.body_offset as usize;
    let end = start.checked_add(col.value_len as usize)?;
    if end > data.len() {
        return None;
    }
    decode_value(col.serial_type, &data[start..end], profile_enabled)
}

/// Decode a single column, reusing the previous row's cached `Arc` when the
/// raw bytes are identical (bd-db300.4.4.2 — K1 copy elimination).
///
/// For Text/Blob columns that repeat across consecutive rows (common in
/// JOINs, GROUP BY, denormalized tables), this avoids the malloc+memcpy+free
/// cycle by returning `Arc::clone` of the existing cached value instead of
/// allocating a fresh `Arc::from(bytes)`.
///
/// Cost when values match: one `memcmp(len)` + `Arc::clone` (~5ns).
/// Cost when values differ: one `memcmp(len)` + normal `decode_value`.
/// Scalars (Integer, Float, Null): no overhead — hint check is skipped.
#[inline]
pub fn decode_column_from_offset_reuse(
    data: &[u8],
    col: &ColumnOffset,
    hint: Option<&SqliteValue>,
    profile_enabled: bool,
) -> Option<SqliteValue> {
    let start = col.body_offset as usize;
    let end = start.checked_add(col.value_len as usize)?;
    if end > data.len() {
        return None;
    }
    let bytes = &data[start..end];

    // Fast path: if the hint's raw bytes match, reuse its Arc allocation.
    if let Some(hint) = hint {
        match (classify_serial_type(col.serial_type), hint) {
            (SerialTypeClass::Text, SqliteValue::Text(arc)) if arc.as_bytes() == bytes => {
                if profile_enabled {
                    note_decoded_value(hint);
                }
                return Some(SqliteValue::Text(arc.clone()));
            }
            (SerialTypeClass::Blob, SqliteValue::Blob(arc)) if arc.as_ref() == bytes => {
                if profile_enabled {
                    note_decoded_value(hint);
                }
                return Some(SqliteValue::Blob(Arc::clone(arc)));
            }
            _ => {}
        }
    }

    decode_value(col.serial_type, bytes, profile_enabled)
}

/// Caller-owned scratch for lazy record decode and row materialization.
///
/// This scratch is intended to be owned by a single cursor-like object and
/// reused only while that owner remains positioned on the same physical record
/// image. Callers must invalidate it when the row image changes.
///
/// The scratch chooses a cursor-local reusable buffer model rather than an
/// arena model:
/// - `header_offsets` caches the parsed record layout for lazy decode
/// - `values` reuses existing `SqliteValue` backing storage across decodes
/// - `decoded_mask` tracks which columns have been materialized for narrow rows
///
/// Nothing in this type is shared across cursors, statements, or transactions.
#[derive(Debug, Default, Clone)]
pub struct RecordDecodeScratch {
    header_offsets: Vec<ColumnOffset>,
    values: Vec<SqliteValue>,
    decoded_mask: u64,
}

impl RecordDecodeScratch {
    /// Prepare the scratch for a new serialized record.
    ///
    /// Returns `Some(true)` when the record is eagerly decoded because it has
    /// more than 64 columns, `Some(false)` when lazy decode remains active, or
    /// `None` when the record is malformed.
    #[must_use]
    pub fn prepare_for_record(&mut self, record: &[u8]) -> Option<bool> {
        let col_count = match parse_record_header_into(record, &mut self.header_offsets) {
            Some(col_count) => col_count,
            None => {
                self.invalidate();
                return None;
            }
        };
        if col_count > 64 {
            self.values.clear();
            if parse_record_into(record, &mut self.values).is_none() {
                self.invalidate();
                return None;
            }
            self.decoded_mask = u64::MAX;
            Some(true)
        } else {
            if self.values.len() > col_count {
                self.values.truncate(col_count);
            } else if self.values.len() < col_count {
                self.values.resize(col_count, SqliteValue::Null);
            }
            self.decoded_mask = 0;
            Some(false)
        }
    }

    /// Prepare the scratch from a partial record prefix.
    ///
    /// This parses only the record header and sizes the reusable value slots
    /// so callers can later fetch and decode just the requested columns.
    #[must_use]
    pub fn prepare_for_record_prefix(&mut self, record: &[u8]) -> Option<usize> {
        let col_count = match parse_record_header_prefix_into(record, &mut self.header_offsets) {
            Some(col_count) => col_count,
            None => {
                self.invalidate();
                return None;
            }
        };

        if self.values.len() > col_count {
            self.values.truncate(col_count);
        } else if self.values.len() < col_count {
            self.values.resize(col_count, SqliteValue::Null);
        }
        self.decoded_mask = 0;
        Some(col_count)
    }

    /// Drop the cached layout and decoded values while preserving capacity for reuse.
    pub fn invalidate(&mut self) {
        self.header_offsets.clear();
        self.values.clear();
        self.decoded_mask = 0;
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.header_offsets.is_empty()
    }

    #[must_use]
    pub fn column_count(&self) -> usize {
        self.header_offsets.len()
    }

    #[must_use]
    pub fn column_offset(&self, idx: usize) -> Option<&ColumnOffset> {
        self.header_offsets.get(idx)
    }

    #[must_use]
    pub fn cached_value(&self, idx: usize) -> Option<&SqliteValue> {
        self.values.get(idx)
    }

    #[must_use]
    pub fn cached_value_ready(&self, idx: usize) -> bool {
        if idx < 64 {
            self.decoded_mask & (1_u64 << idx) != 0
        } else {
            self.decoded_mask == u64::MAX
        }
    }

    pub fn cache_decoded(&mut self, idx: usize, value: SqliteValue) {
        if idx >= self.values.len() {
            self.values.resize(idx + 1, SqliteValue::Null);
        }
        self.values[idx] = value;
        if idx < 64 {
            self.decoded_mask |= 1_u64 << idx;
        }
    }
}

/// Decode only the first `n` columns from a serialized record.
///
/// This is the sort-key extraction path: for ORDER BY on 2 columns of a
/// 20-column record, we decode only 2 columns instead of 20.  The raw
/// blob is kept separately for output.
#[allow(clippy::cast_possible_truncation)]
pub fn parse_record_prefix(data: &[u8], max_cols: usize) -> Option<Vec<SqliteValue>> {
    if max_cols == 0 {
        return Some(Vec::new());
    }
    if data.is_empty() {
        return None;
    }

    let (header_size_u64, hdr_varint_len) = read_varint(data)?;
    let header_size = usize::try_from(header_size_u64).unwrap_or(usize::MAX);
    if header_size > data.len() || header_size < hdr_varint_len {
        return None;
    }

    let mut values = Vec::with_capacity(max_cols.min(16));
    let mut offset = hdr_varint_len;
    let mut body_offset = header_size;
    let profile_enabled = record_profile_enabled();

    while offset < header_size && values.len() < max_cols {
        let (serial_type, consumed) = read_varint(&data[offset..header_size])?;
        offset += consumed;

        let value_len_u64 = serial_type_len(serial_type)?;
        let value_len = usize::try_from(value_len_u64).unwrap_or(usize::MAX);
        let end = body_offset.checked_add(value_len)?;
        if end > data.len() {
            return None;
        }

        let value = decode_value(serial_type, &data[body_offset..end], profile_enabled)?;
        values.push(value);
        body_offset = end;
    }

    Some(values)
}

/// Serialize a list of `SqliteValue` into the SQLite record format.
pub fn serialize_record(values: &[SqliteValue]) -> Vec<u8> {
    serialize_record_iter(values.iter())
}

/// Serialize a list of `SqliteValue` references into the SQLite record format.
pub fn serialize_record_refs(values: &[&SqliteValue]) -> Vec<u8> {
    serialize_record_iter(values.iter().copied())
}

/// Core serialization logic.
///
/// Computes each column's serial type **once** into a small stack buffer
/// (up to 32 columns without heap allocation), then writes header + body
/// in a single pass.  This replaces the prior 3-pass design that called
/// `serial_type_for_value()` three times per column.
#[allow(clippy::cast_possible_truncation)]
pub fn serialize_record_iter<'a, I>(values: I) -> Vec<u8>
where
    I: Iterator<Item = &'a SqliteValue> + Clone,
{
    // ── Phase 1: compute serial types + sizes (one call per column) ──
    // Stack-allocate for records up to 32 columns (covers 95%+ of tables).
    let mut serial_types_buf: [u64; 32] = [0; 32];
    let mut serial_types_heap: Vec<u64> = Vec::new();
    let mut n_cols: usize = 0;
    let mut header_content_size: usize = 0;
    let mut body_size: usize = 0;

    for v in values.clone() {
        let st = serial_type_for_value(v);
        if n_cols < 32 {
            serial_types_buf[n_cols] = st;
        } else {
            if serial_types_heap.is_empty() {
                serial_types_heap.extend_from_slice(&serial_types_buf);
            }
            serial_types_heap.push(st);
        }
        header_content_size += varint_len(st);
        body_size += serial_type_len(st).unwrap_or(0) as usize;
        n_cols += 1;
    }

    let serial_types: &[u64] = if n_cols <= 32 {
        &serial_types_buf[..n_cols]
    } else {
        &serial_types_heap
    };

    // ── Phase 2: single-pass write header + body ─────────────────────
    let header_size = compute_header_size(header_content_size);
    let total_size = header_size + body_size;
    let mut buf = vec![0u8; total_size];

    // Write header-size varint.
    let mut offset = write_varint(&mut buf, header_size as u64);

    // Write serial type varints (from cached array, not recomputed).
    for &st in serial_types {
        offset += write_varint(&mut buf[offset..], st);
    }
    debug_assert_eq!(offset, header_size);

    // Write encoded values (serial types read from cache).
    for (v, &st) in values.zip(serial_types.iter()) {
        let value_len = serial_type_len(st).unwrap_or(0) as usize;
        encode_value(v, st, &mut buf[offset..offset + value_len]);
        offset += value_len;
    }

    buf
}

/// Serialize a record into a reusable buffer, avoiding allocation.
///
/// Same logic as `serialize_record_iter` but writes into `buf` (clearing it
/// first and reusing its capacity). This eliminates the per-row `Vec<u8>`
/// allocation in the `MakeRecord` opcode hot path.
pub fn serialize_record_iter_into<'a, I>(values: I, buf: &mut Vec<u8>)
where
    I: Iterator<Item = &'a SqliteValue> + Clone,
{
    // ── Phase 1: compute serial types + sizes (one call per column) ──
    let mut serial_types_buf: [u64; 32] = [0; 32];
    let mut serial_types_heap: Vec<u64> = Vec::new();
    let mut n_cols: usize = 0;
    let mut header_content_size: usize = 0;
    let mut body_size: usize = 0;

    for v in values.clone() {
        let st = serial_type_for_value(v);
        if n_cols < 32 {
            serial_types_buf[n_cols] = st;
        } else {
            if serial_types_heap.is_empty() {
                serial_types_heap.extend_from_slice(&serial_types_buf);
            }
            serial_types_heap.push(st);
        }
        header_content_size += varint_len(st);
        body_size += serial_type_len(st).unwrap_or(0) as usize;
        n_cols += 1;
    }

    let serial_types: &[u64] = if n_cols <= 32 {
        &serial_types_buf[..n_cols]
    } else {
        &serial_types_heap
    };

    // ── Phase 2: single-pass write header + body ─────────────────────
    let header_size = compute_header_size(header_content_size);
    let total_size = header_size + body_size;
    if buf.len() < total_size {
        buf.resize(total_size, 0);
    } else {
        buf.truncate(total_size);
    }

    let mut offset = write_varint(buf, header_size as u64);
    for &st in serial_types {
        offset += write_varint(&mut buf[offset..], st);
    }
    debug_assert_eq!(offset, header_size);

    for (v, &st) in values.zip(serial_types.iter()) {
        let value_len = serial_type_len(st).unwrap_or(0) as usize;
        encode_value(v, st, &mut buf[offset..offset + value_len]);
        offset += value_len;
    }
}

/// Compute the total header size (including the header-size varint itself).
#[allow(clippy::cast_possible_truncation)]
fn compute_header_size(content_size: usize) -> usize {
    // Start with a guess and iterate.
    let mut header_size = content_size + 1; // +1 for the minimum varint
    loop {
        let needed = varint_len(header_size as u64) + content_size;
        if needed <= header_size {
            return header_size;
        }
        header_size = needed;
    }
}

/// Determine the serial type for a `SqliteValue`.
#[allow(clippy::cast_possible_truncation)]
fn serial_type_for_value(value: &SqliteValue) -> u64 {
    match value {
        SqliteValue::Null => 0,
        SqliteValue::Integer(i) => serial_type_for_integer(*i),
        // SQLite normalizes NaN to NULL for deterministic storage.
        SqliteValue::Float(f) => {
            if f.is_nan() {
                0
            } else {
                7
            }
        }
        SqliteValue::Text(s) => serial_type_for_text(s.len() as u64),
        SqliteValue::Blob(b) => serial_type_for_blob(b.len() as u64),
    }
}

/// Decode a value from its serial type and raw bytes.
///
/// Public so that [`RecordOffsetTable`] consumers can perform lazy
/// per-column decoding without re-parsing the header.
#[allow(clippy::cast_possible_truncation)]
pub fn decode_value(serial_type: u64, bytes: &[u8], profile_enabled: bool) -> Option<SqliteValue> {
    let value = match classify_serial_type(serial_type) {
        SerialTypeClass::Null => Some(SqliteValue::Null),
        SerialTypeClass::Zero => Some(SqliteValue::Integer(0)),
        SerialTypeClass::One => Some(SqliteValue::Integer(1)),
        SerialTypeClass::Integer => {
            let value = decode_big_endian_signed(bytes);
            Some(SqliteValue::Integer(value))
        }
        SerialTypeClass::Float => {
            if bytes.len() != 8 {
                return None;
            }
            let bits = u64::from_be_bytes(bytes.try_into().ok()?);
            let value = f64::from_bits(bits);
            if value.is_nan() {
                Some(SqliteValue::Null)
            } else {
                Some(SqliteValue::Float(value))
            }
        }
        SerialTypeClass::Text => std::str::from_utf8(bytes)
            .ok()
            .map(|text| SqliteValue::Text(text.into())),
        SerialTypeClass::Blob => Some(SqliteValue::Blob(Arc::from(bytes))),
        SerialTypeClass::Reserved => None,
    };

    if profile_enabled && let Some(value) = value.as_ref() {
        note_decoded_value(value);
    }
    value
}

fn decode_value_into(
    serial_type: u64,
    bytes: &[u8],
    slot: &mut SqliteValue,
    profile_enabled: bool,
) -> Option<()> {
    match classify_serial_type(serial_type) {
        SerialTypeClass::Null => {
            *slot = SqliteValue::Null;
        }
        SerialTypeClass::Zero => {
            *slot = SqliteValue::Integer(0);
        }
        SerialTypeClass::One => {
            *slot = SqliteValue::Integer(1);
        }
        SerialTypeClass::Integer => {
            *slot = SqliteValue::Integer(decode_big_endian_signed(bytes));
        }
        SerialTypeClass::Float => {
            if bytes.len() != 8 {
                return None;
            }
            let bits = u64::from_be_bytes(bytes.try_into().ok()?);
            let value = f64::from_bits(bits);
            *slot = if value.is_nan() {
                SqliteValue::Null
            } else {
                SqliteValue::Float(value)
            };
        }
        SerialTypeClass::Text => {
            let text = std::str::from_utf8(bytes).ok()?;
            // bd-db300.4.4.2 K1: reuse existing Arc if the raw bytes match,
            // avoiding malloc+memcpy for duplicate text across consecutive rows.
            if let SqliteValue::Text(existing) = slot {
                if existing.as_bytes() == bytes {
                    // Existing Arc already holds identical content — keep it.
                    if profile_enabled {
                        note_decoded_value(slot);
                    }
                    return Some(());
                }
            }
            *slot = SqliteValue::Text(text.into());
        }
        SerialTypeClass::Blob => {
            // bd-db300.4.4.2 K1: same reuse optimization for blobs.
            if let SqliteValue::Blob(existing) = slot {
                if existing.as_ref() == bytes {
                    if profile_enabled {
                        note_decoded_value(slot);
                    }
                    return Some(());
                }
            }
            *slot = SqliteValue::Blob(Arc::from(bytes));
        }
        SerialTypeClass::Reserved => return None,
    }

    if profile_enabled {
        note_decoded_value(slot);
    }
    Some(())
}

/// Decode a big-endian signed integer of 1-8 bytes.
#[allow(clippy::cast_possible_wrap)]
fn decode_big_endian_signed(bytes: &[u8]) -> i64 {
    match bytes.len() {
        0 => 0,
        1 => bytes[0] as i8 as i64,
        2 => i16::from_be_bytes(bytes.try_into().unwrap()) as i64,
        3 => {
            let mut buf = [if bytes[0] & 0x80 != 0 { 0xFF } else { 0 }; 4];
            buf[1..4].copy_from_slice(bytes);
            i32::from_be_bytes(buf) as i64
        }
        4 => i32::from_be_bytes(bytes.try_into().unwrap()) as i64,
        6 => {
            let mut buf = [if bytes[0] & 0x80 != 0 { 0xFF } else { 0 }; 8];
            buf[2..8].copy_from_slice(bytes);
            i64::from_be_bytes(buf)
        }
        8 => i64::from_be_bytes(bytes.try_into().unwrap()),
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

/// Encode a `SqliteValue` into its serial type byte representation.
fn encode_value(value: &SqliteValue, serial_type: u64, buf: &mut [u8]) {
    match value {
        SqliteValue::Null => {} // serial type 0: no data
        SqliteValue::Integer(i) => {
            if classify_serial_type(serial_type) == SerialTypeClass::Integer {
                let len = buf.len();
                let bytes = i.to_be_bytes();
                // Take the least significant `len` bytes.
                buf.copy_from_slice(&bytes[8 - len..]);
            }
            // Zero and One serial types have no data bytes.
        }
        SqliteValue::Float(f) => {
            if f.is_nan() {
                return;
            }
            let bits = f.to_bits();
            buf.copy_from_slice(&bits.to_be_bytes());
        }
        SqliteValue::Text(s) => {
            buf.copy_from_slice(s.as_bytes());
        }
        SqliteValue::Blob(b) => {
            buf.copy_from_slice(b);
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::approx_constant)]
mod tests {
    use super::*;
    use crate::value::SmallText;

    #[test]
    fn empty_record() {
        let data = serialize_record(&[]);
        assert!(!data.is_empty());
        let values = parse_record(&data).unwrap();
        assert!(values.is_empty());
    }

    #[test]
    fn null_record() {
        let values = vec![SqliteValue::Null];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].is_null());
    }

    #[test]
    fn integer_zero() {
        let values = vec![SqliteValue::Integer(0)];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_integer(), Some(0));
    }

    #[test]
    fn integer_one() {
        let values = vec![SqliteValue::Integer(1)];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_integer(), Some(1));
    }

    #[test]
    fn parse_record_into_decodes_text_correctly() {
        let first = serialize_record(&[SqliteValue::Text(SmallText::new("hello reusable buffer"))]);
        let second = serialize_record(&[SqliteValue::Text(SmallText::new("shorter"))]);
        let mut values = Vec::new();

        parse_record_into(&first, &mut values).expect("first decode");
        assert_eq!(values[0].as_text(), Some("hello reusable buffer"));

        parse_record_into(&second, &mut values).expect("second decode");
        assert_eq!(values[0].as_text(), Some("shorter"));
    }

    #[test]
    fn parse_record_into_decodes_blob_correctly() {
        let first = serialize_record(&[SqliteValue::Blob(Arc::from(
            [1u8, 2, 3, 4, 5, 6, 7, 8].as_slice(),
        ))]);
        let second = serialize_record(&[SqliteValue::Blob(Arc::from([9u8, 10, 11].as_slice()))]);
        let mut values = Vec::new();

        parse_record_into(&first, &mut values).expect("first decode");
        assert_eq!(values[0].as_blob(), Some(&[1u8, 2, 3, 4, 5, 6, 7, 8][..]));

        parse_record_into(&second, &mut values).expect("second decode");
        assert_eq!(values[0].as_blob(), Some(&[9u8, 10, 11][..]));
    }

    #[test]
    fn record_decode_scratch_reuses_small_record_state() {
        let record = serialize_record(&[
            SqliteValue::Integer(7),
            SqliteValue::Text(SmallText::new("scratch-owned")),
        ]);
        let mut scratch = RecordDecodeScratch::default();

        let eager = scratch
            .prepare_for_record(&record)
            .expect("record layout should decode");
        assert!(!eager);
        assert_eq!(scratch.column_count(), 2);
        assert!(!scratch.cached_value_ready(1));

        let decoded = decode_column_from_offset(
            &record,
            scratch
                .column_offset(1)
                .expect("column offset should exist"),
            false,
        )
        .expect("column should decode");
        scratch.cache_decoded(1, decoded);

        assert!(scratch.cached_value_ready(1));
        assert_eq!(
            scratch.cached_value(1).and_then(SqliteValue::as_text),
            Some("scratch-owned")
        );

        scratch.invalidate();
        assert!(scratch.is_empty());
        assert!(!scratch.cached_value_ready(1));
        assert!(scratch.cached_value(1).is_none());
    }

    #[test]
    fn record_decode_scratch_eagerly_materializes_wide_records() {
        let values: Vec<_> = (0_i64..65).map(SqliteValue::Integer).collect();
        let record = serialize_record(&values);
        let mut scratch = RecordDecodeScratch::default();

        let eager = scratch
            .prepare_for_record(&record)
            .expect("wide record layout should decode");
        assert!(eager);
        assert_eq!(scratch.column_count(), 65);
        assert!(scratch.cached_value_ready(64));
        assert_eq!(
            scratch.cached_value(64).and_then(SqliteValue::as_integer),
            Some(64)
        );
    }

    #[test]
    fn parse_record_header_prefix_accepts_truncated_body() {
        let record = serialize_record(&[
            SqliteValue::Integer(42),
            SqliteValue::Text(SmallText::new("hello")),
            SqliteValue::Blob(Arc::from([1_u8, 2, 3, 4].as_slice())),
        ]);
        let (header_size_u64, _) = read_varint(&record).expect("record header should parse");
        let header_size = usize::try_from(header_size_u64).expect("header size should fit");
        let prefix = &record[..header_size + 1];

        let mut offsets = Vec::new();
        let count = parse_record_header_prefix_into(prefix, &mut offsets)
            .expect("prefix parser should accept truncated body");

        assert_eq!(count, 3);
        assert_eq!(offsets[0].body_offset as usize, header_size);
        assert_eq!(offsets[1].body_offset as usize, header_size + 1);
        assert_eq!(offsets[2].body_offset as usize, header_size + 6);
    }

    #[test]
    fn integer_small() {
        let values = vec![SqliteValue::Integer(42)];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_integer(), Some(42));
    }

    #[test]
    fn integer_negative() {
        let values = vec![SqliteValue::Integer(-1)];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_integer(), Some(-1));
    }

    #[test]
    fn integer_large() {
        for &val in &[i64::MIN, i64::MAX, 0x7FFF_FFFF_FFFF, -0x7FFF_FFFF_FFFF] {
            let values = vec![SqliteValue::Integer(val)];
            let data = serialize_record(&values);
            let parsed = parse_record(&data).unwrap();
            assert_eq!(
                parsed[0].as_integer(),
                Some(val),
                "roundtrip failed for {val}"
            );
        }
    }

    #[test]
    fn float_value() {
        let values = vec![SqliteValue::Float(3.14)];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_float(), Some(3.14));
    }

    #[test]
    fn float_special_values() {
        for &val in &[0.0, -0.0, f64::INFINITY, f64::NEG_INFINITY] {
            let values = vec![SqliteValue::Float(val)];
            let data = serialize_record(&values);
            let parsed = parse_record(&data).unwrap();
            assert_eq!(parsed[0].as_float().unwrap().to_bits(), val.to_bits());
        }
    }

    #[test]
    fn float_nan_normalized_to_null_for_storage() {
        let values = vec![SqliteValue::Float(f64::NAN)];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].is_null());
    }

    #[test]
    fn text_value() {
        let values = vec![SqliteValue::Text(SmallText::new("hello world"))];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_text(), Some("hello world"));
    }

    #[test]
    fn text_empty() {
        let values = vec![SqliteValue::Text(SmallText::new(""))];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_text(), Some(""));
    }

    #[test]
    fn blob_value() {
        let values = vec![SqliteValue::Blob(Arc::from(
            [0xDEu8, 0xAD, 0xBE, 0xEF].as_slice(),
        ))];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_blob(), Some(&[0xDE, 0xAD, 0xBE, 0xEF][..]));
    }

    #[test]
    fn blob_empty() {
        let values = vec![SqliteValue::Blob(Arc::from(&[] as &[u8]))];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_blob(), Some(&[][..]));
    }

    #[test]
    fn mixed_record() {
        let values = vec![
            SqliteValue::Integer(42),
            SqliteValue::Text(SmallText::new("hello")),
            SqliteValue::Null,
            SqliteValue::Float(2.718),
            SqliteValue::Blob(Arc::from([1u8, 2, 3].as_slice())),
            SqliteValue::Integer(0),
            SqliteValue::Integer(1),
        ];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();

        assert_eq!(parsed.len(), 7);
        assert_eq!(parsed[0].as_integer(), Some(42));
        assert_eq!(parsed[1].as_text(), Some("hello"));
        assert!(parsed[2].is_null());
        assert_eq!(parsed[3].as_float(), Some(2.718));
        assert_eq!(parsed[4].as_blob(), Some(&[1, 2, 3][..]));
        assert_eq!(parsed[5].as_integer(), Some(0));
        assert_eq!(parsed[6].as_integer(), Some(1));
    }

    #[test]
    fn test_serial_type_roundtrip_all_categories() {
        let values = vec![
            SqliteValue::Null,
            SqliteValue::Integer(i64::from(i8::MIN)),
            SqliteValue::Integer(i64::from(i16::MIN)),
            SqliteValue::Integer(-8_388_608), // 24-bit boundary
            SqliteValue::Integer(i64::from(i32::MIN)),
            SqliteValue::Integer(-140_737_488_355_328), // 48-bit boundary
            SqliteValue::Integer(i64::MIN),
            SqliteValue::Float(-1234.5),
            SqliteValue::Integer(0),
            SqliteValue::Integer(1),
            SqliteValue::Blob(Arc::from([0xDEu8, 0xAD, 0xBE, 0xEF].as_slice())),
            SqliteValue::Text(SmallText::new("serial-type-text")),
        ];

        let encoded = serialize_record(&values);
        let parsed = parse_record(&encoded).expect("record must decode");
        assert_eq!(parsed.len(), values.len());

        assert!(parsed[0].is_null());
        assert_eq!(parsed[1].as_integer(), Some(i64::from(i8::MIN)));
        assert_eq!(parsed[2].as_integer(), Some(i64::from(i16::MIN)));
        assert_eq!(parsed[3].as_integer(), Some(-8_388_608));
        assert_eq!(parsed[4].as_integer(), Some(i64::from(i32::MIN)));
        assert_eq!(parsed[5].as_integer(), Some(-140_737_488_355_328));
        assert_eq!(parsed[6].as_integer(), Some(i64::MIN));
        assert_eq!(
            parsed[7].as_float().map(f64::to_bits),
            Some((-1234.5f64).to_bits())
        );
        assert_eq!(parsed[8].as_integer(), Some(0));
        assert_eq!(parsed[9].as_integer(), Some(1));
        assert_eq!(parsed[10].as_blob(), Some(&[0xDE, 0xAD, 0xBE, 0xEF][..]));
        assert_eq!(parsed[11].as_text(), Some("serial-type-text"));
    }

    #[test]
    fn integer_size_boundaries() {
        // Verify that integers at size boundaries roundtrip correctly.
        let test_values: &[i64] = &[
            0,
            1,
            -1,
            127,
            -128,
            128,
            -129,
            32767,
            -32768,
            32768,
            8_388_607,
            -8_388_608,
            8_388_608,
            2_147_483_647,
            -2_147_483_648,
            2_147_483_648,
            0x0000_7FFF_FFFF_FFFF,
            -0x0000_8000_0000_0000,
            0x0000_8000_0000_0000,
            i64::MAX,
            i64::MIN,
        ];

        for &val in test_values {
            let values = vec![SqliteValue::Integer(val)];
            let data = serialize_record(&values);
            let parsed = parse_record(&data).unwrap();
            assert_eq!(
                parsed[0].as_integer(),
                Some(val),
                "roundtrip failed for {val}"
            );
        }
    }

    #[test]
    fn decode_big_endian_signed_cases() {
        assert_eq!(decode_big_endian_signed(&[]), 0);
        assert_eq!(decode_big_endian_signed(&[42]), 42);
        assert_eq!(decode_big_endian_signed(&[0xFF]), -1);
        assert_eq!(decode_big_endian_signed(&[0x00, 0x80]), 128);
        assert_eq!(decode_big_endian_signed(&[0xFF, 0x7F]), -129);
    }

    #[test]
    fn malformed_record_too_short() {
        // Header says it's 10 bytes but we only have 2.
        let data = [10, 0];
        assert!(parse_record(&data).is_none());
    }

    #[test]
    fn malformed_record_body_truncated() {
        // Header has serial type 6 (8-byte int) but body is empty.
        let data = [2, 6]; // header_size=2, serial_type=6, body missing
        assert!(parse_record(&data).is_none());
    }

    #[test]
    fn large_text_roundtrip() {
        let big_text = "x".repeat(10_000);
        let values = vec![SqliteValue::Text(SmallText::new(big_text.as_str()))];
        let data = serialize_record(&values);
        let parsed = parse_record(&data).unwrap();
        assert_eq!(parsed[0].as_text(), Some(big_text.as_str()));
    }

    #[test]
    fn header_size_computation() {
        // For small records, header size varint is 1 byte.
        assert_eq!(compute_header_size(0), 1);
        assert_eq!(compute_header_size(1), 2);
        assert_eq!(compute_header_size(126), 127);
        // At 127+, the header size varint becomes 2 bytes.
        assert_eq!(compute_header_size(127), 129);
    }

    #[test]
    fn parse_empty_data_is_malformed() {
        assert!(parse_record(&[]).is_none());
    }

    #[test]
    fn malformed_record_invalid_utf8_text() {
        let data = [0x02, 0x0F, 0xFF];
        assert!(parse_record(&data).is_none());
    }

    #[test]
    fn malformed_record_rejects_trailing_body_bytes() {
        let data = [0x02, 0x01, 0x2A, 0x63];
        assert!(parse_record(&data).is_none());
    }

    #[test]
    fn record_column_count_matches_encoded_values() {
        let values = vec![
            SqliteValue::Null,
            SqliteValue::Integer(42),
            SqliteValue::Text(SmallText::new("hello")),
            SqliteValue::Blob(Arc::from([1u8, 2, 3].as_slice())),
        ];
        let data = serialize_record(&values);
        assert_eq!(record_column_count(&data), Some(values.len()));
    }

    #[test]
    fn test_record_format_null_vector() {
        let data = serialize_record(&[SqliteValue::Null]);
        assert_eq!(data, vec![0x02, 0x00]);
    }

    #[test]
    fn test_record_format_int8_vector() {
        let data = serialize_record(&[SqliteValue::Integer(42)]);
        assert_eq!(data, vec![0x02, 0x01, 0x2A]);
    }

    #[test]
    fn test_record_format_int16_vector() {
        let data = serialize_record(&[SqliteValue::Integer(256)]);
        assert_eq!(data, vec![0x02, 0x02, 0x01, 0x00]);
    }

    #[test]
    fn test_record_format_int64_vector() {
        let value = 0x0102_0304_0506_0708_i64;
        let data = serialize_record(&[SqliteValue::Integer(value)]);
        assert_eq!(
            data,
            vec![0x02, 0x06, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }

    #[test]
    fn test_record_format_float_vector() {
        let data = serialize_record(&[SqliteValue::Float(3.14)]);
        assert_eq!(
            data,
            vec![0x02, 0x07, 0x40, 0x09, 0x1E, 0xB8, 0x51, 0xEB, 0x85, 0x1F]
        );
    }

    #[test]
    fn test_record_format_zero_one_constants_vectors() {
        let zero = serialize_record(&[SqliteValue::Integer(0)]);
        assert_eq!(zero, vec![0x02, 0x08]);

        let one = serialize_record(&[SqliteValue::Integer(1)]);
        assert_eq!(one, vec![0x02, 0x09]);
    }

    #[test]
    fn test_record_format_text_blob_vectors() {
        let text = serialize_record(&[SqliteValue::Text(SmallText::new("hello"))]);
        assert_eq!(text, vec![0x02, 0x17, 0x68, 0x65, 0x6C, 0x6C, 0x6F]);

        let blob = serialize_record(&[SqliteValue::Blob(Arc::from([0xCAu8, 0xFE].as_slice()))]);
        assert_eq!(blob, vec![0x02, 0x10, 0xCA, 0xFE]);
    }

    #[test]
    fn test_record_format_worked_example_exact_bytes() {
        let values = vec![
            SqliteValue::Integer(42),
            SqliteValue::Text(SmallText::new("hello")),
            SqliteValue::Float(3.14),
            SqliteValue::Null,
            SqliteValue::Blob(Arc::from([0xCAu8, 0xFE].as_slice())),
        ];
        let data = serialize_record(&values);
        let expected = vec![
            0x06, 0x01, 0x17, 0x07, 0x00, 0x10, 0x2A, 0x68, 0x65, 0x6C, 0x6C, 0x6F, 0x40, 0x09,
            0x1E, 0xB8, 0x51, 0xEB, 0x85, 0x1F, 0xCA, 0xFE,
        ];
        assert_eq!(data, expected);
        assert_eq!(data.len(), 22);
    }

    #[test]
    fn test_record_header_size_includes_self_for_one_and_ten_columns() {
        let one_col = serialize_record(&[SqliteValue::Integer(42)]);
        let (one_header_size, one_consumed) = read_varint(&one_col).expect("valid one-col header");
        assert_eq!(one_consumed, 1);
        assert_eq!(one_header_size, 2);

        let ten_values: Vec<SqliteValue> = (2_i64..=11).map(SqliteValue::Integer).collect();
        let ten_col = serialize_record(&ten_values);
        let (ten_header_size, ten_consumed) = read_varint(&ten_col).expect("valid ten-col header");
        assert_eq!(ten_consumed, 1);
        assert_eq!(ten_header_size, 11);
        assert!(ten_col[1..11].iter().all(|&serial| serial == 0x01));
    }

    // -----------------------------------------------------------------------
    // bd-2sm1 §17.2 proptest: record format round-trip
    // -----------------------------------------------------------------------

    use proptest::prelude::*;

    /// Strategy to generate an arbitrary `SqliteValue`.
    fn arb_sqlite_value() -> BoxedStrategy<SqliteValue> {
        prop_oneof![
            5 => Just(SqliteValue::Null),
            10 => any::<i64>().prop_map(SqliteValue::Integer),
            5 => prop_oneof![
                Just(0_i64),
                Just(1_i64),
                Just(-1_i64),
                Just(127_i64),
                Just(-128_i64),
                Just(128_i64),
                Just(32767_i64),
                Just(-32768_i64),
                Just(8_388_607_i64),
                Just(-8_388_608_i64),
                Just(2_147_483_647_i64),
                Just(-2_147_483_648_i64),
                Just(i64::MAX),
                Just(i64::MIN),
            ].prop_map(SqliteValue::Integer),
            // Exclude NaN/Inf since they roundtrip but NaN != NaN in PartialEq.
            5 => (-1e15_f64..1e15_f64).prop_map(SqliteValue::Float),
            5 => prop_oneof![
                Just(0.0_f64),
                Just(-0.0_f64),
                Just(1.0_f64),
                Just(-1.0_f64),
                Just(3.14_f64),
                Just(f64::MAX),
                Just(f64::MIN),
                Just(f64::MIN_POSITIVE),
            ].prop_map(SqliteValue::Float),
            10 => "[a-zA-Z0-9 _]{0,200}".prop_map(|s: String| SqliteValue::Text(SmallText::from_string(s))),
            5 => proptest::collection::vec(any::<u8>(), 0..200)
                .prop_map(|v: Vec<u8>| SqliteValue::Blob(Arc::from(v.as_slice()))),
        ]
        .boxed()
    }

    /// Bitwise equality for SqliteValue (handles NaN and -0.0 correctly).
    fn values_bitwise_eq(a: &SqliteValue, b: &SqliteValue) -> bool {
        match (a, b) {
            (SqliteValue::Null, SqliteValue::Null) => true,
            (SqliteValue::Integer(x), SqliteValue::Integer(y)) => x == y,
            (SqliteValue::Float(x), SqliteValue::Float(y)) => x.to_bits() == y.to_bits(),
            (SqliteValue::Text(x), SqliteValue::Text(y)) => x == y,
            (SqliteValue::Blob(x), SqliteValue::Blob(y)) => x == y,
            _ => false,
        }
    }

    #[test]
    fn record_profile_scope_breakdown_tracks_and_restores_nested_scopes() {
        reset_record_profile();
        set_record_profile_enabled(true);

        let encoded = serialize_record(&[
            SqliteValue::Integer(7),
            SqliteValue::Text(SmallText::new("alpha")),
        ]);

        {
            let _outer = enter_record_profile_scope(RecordProfileScope::CoreConnection);
            let _ = parse_record(&encoded).expect("core decode should succeed");
            {
                let _inner = enter_record_profile_scope(RecordProfileScope::VdbeEngine);
                let _ = parse_record_column(&encoded, 1).expect("column decode should succeed");
            }
            let mut scratch = Vec::new();
            parse_record_into(&encoded, &mut scratch).expect("reused decode should succeed");
        }

        let snapshot = record_profile_snapshot();
        assert_eq!(snapshot.parse_record_calls, 1);
        // Full-row decode currently delegates to `parse_record_into`, so the
        // aggregate "into" counter includes both the explicit reuse pass and
        // the nested call performed by `parse_record`.
        assert_eq!(snapshot.parse_record_into_calls, 2);
        assert_eq!(snapshot.parse_record_column_calls, 1);
        assert_eq!(
            snapshot
                .callsite_breakdown
                .core_connection
                .parse_record_calls,
            1
        );
        assert_eq!(
            snapshot
                .callsite_breakdown
                .core_connection
                .parse_record_into_calls,
            2
        );
        assert_eq!(
            snapshot
                .callsite_breakdown
                .vdbe_engine
                .parse_record_column_calls,
            1
        );
        assert_eq!(
            snapshot.callsite_breakdown.unattributed.parse_record_calls,
            0
        );

        set_record_profile_enabled(false);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(2000))]

        /// INV-PBT-3: Record encode/decode round-trip for arbitrary value vectors.
        #[test]
        fn prop_record_roundtrip_arbitrary(
            values in proptest::collection::vec(arb_sqlite_value(), 0..100)
        ) {
            let encoded = serialize_record(&values);
            let decoded = parse_record(&encoded)
                .expect("serialize_record output must always parse");
            prop_assert_eq!(
                values.len(),
                decoded.len(),
                "bead_id=bd-2sm1 case=record_roundtrip_len_mismatch expected={} got={}",
                values.len(),
                decoded.len()
            );
            for (i, (orig, parsed)) in values.iter().zip(decoded.iter()).enumerate() {
                prop_assert!(
                    values_bitwise_eq(orig, parsed),
                    "bead_id=bd-2sm1 case=record_roundtrip_value_mismatch col={} orig={:?} parsed={:?}",
                    i,
                    orig,
                    parsed
                );
            }
        }

        /// INV-PBT-3: Edge cases at varint encoding boundaries.
        #[test]
        fn prop_record_roundtrip_varint_boundaries(
            n_cols in 1_usize..50
        ) {
            // Use values at varint boundaries to stress header encoding.
            let boundary_values: Vec<i64> = vec![
                0, 1, -1, 127, -128, 128, -129, 32767, -32768, 32768,
                8_388_607, -8_388_608, 2_147_483_647, -2_147_483_648,
                i64::MAX, i64::MIN,
            ];
            let values: Vec<SqliteValue> = (0..n_cols)
                .map(|i| SqliteValue::Integer(boundary_values[i % boundary_values.len()]))
                .collect();
            let encoded = serialize_record(&values);
            let decoded = parse_record(&encoded).expect("valid boundary record");
            prop_assert_eq!(
                values.len(),
                decoded.len(),
                "bead_id=bd-2sm1 case=varint_boundary_len"
            );
            for (i, (orig, parsed)) in values.iter().zip(decoded.iter()).enumerate() {
                prop_assert!(
                    values_bitwise_eq(orig, parsed),
                    "bead_id=bd-2sm1 case=varint_boundary_mismatch col={}",
                    i
                );
            }
        }

        /// INV-PBT-3: Empty and single-value records.
        #[test]
        fn prop_record_roundtrip_single_value(value in arb_sqlite_value()) {
            let values = vec![value];
            let encoded = serialize_record(&values);
            let decoded = parse_record(&encoded).expect("valid single-value record");
            prop_assert_eq!(decoded.len(), 1, "bead_id=bd-2sm1 case=single_value_len");
            prop_assert!(
                values_bitwise_eq(&values[0], &decoded[0]),
                "bead_id=bd-2sm1 case=single_value_mismatch orig={:?} parsed={:?}",
                values[0],
                decoded[0]
            );
        }

        /// INV-PBT-4: Lazy header-only parse + per-column decode matches full parse.
        #[test]
        fn prop_lazy_decode_matches_full(
            values in proptest::collection::vec(arb_sqlite_value(), 1..50)
        ) {
            let encoded = serialize_record(&values);
            let full = parse_record(&encoded).expect("full parse must succeed");
            let mut offsets = Vec::new();
            let col_count = parse_record_header_into(&encoded, &mut offsets)
                .expect("header-only parse must succeed");
            prop_assert_eq!(col_count, full.len(), "column count mismatch");
            for i in 0..col_count {
                let lazy_val = decode_column_from_offset(&encoded, &offsets[i], false)
                    .expect("lazy decode must succeed");
                prop_assert!(
                    values_bitwise_eq(&full[i], &lazy_val),
                    "lazy decode mismatch at col {i}: full={:?} lazy={:?}",
                    full[i],
                    lazy_val,
                );
            }
        }
    }
}
