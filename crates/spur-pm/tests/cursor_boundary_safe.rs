//! Unit tests for the boundary-safe `PollCursor` type introduced in v0a.2 Task 10.
//!
//! These tests do not require `br` to be installed; they exercise the pure-Rust
//! predicate logic and serde round-trips directly.

use chrono::{DateTime, Utc};
use spur_pm::PollCursor;
use std::collections::HashSet;

// ─── helpers ─────────────────────────────────────────────────────────────────

fn ts(rfc: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc).unwrap().with_timezone(&Utc)
}

fn cursor_with_ids(ts_str: &str, ids: &[&str]) -> PollCursor {
    PollCursor {
        ts: ts(ts_str),
        ids_at_boundary: ids.iter().map(|s| s.to_string()).collect(),
    }
}

// ─── PollCursor::allows predicate ────────────────────────────────────────────

/// An item strictly newer than the cursor ts always passes.
#[test]
fn allows_strictly_newer() {
    let c = cursor_with_ids("2026-04-20T12:00:00Z", &["bd-1"]);
    assert!(c.allows("bd-1", ts("2026-04-20T12:00:01Z")));
    assert!(c.allows("bd-2", ts("2026-04-20T13:00:00Z")));
}

/// An item already in `ids_at_boundary` at the cursor ts is suppressed.
#[test]
fn suppresses_boundary_id_replay() {
    let c = cursor_with_ids("2026-04-20T12:00:00Z", &["bd-1", "bd-2"]);
    assert!(!c.allows("bd-1", ts("2026-04-20T12:00:00Z")));
    assert!(!c.allows("bd-2", ts("2026-04-20T12:00:00Z")));
}

/// A new id at the boundary ts (not in ids_at_boundary) does pass.
#[test]
fn allows_new_id_at_boundary_ts() {
    let c = cursor_with_ids("2026-04-20T12:00:00Z", &["bd-1"]);
    // bd-2 has same ts but was NOT in the boundary set — should pass.
    assert!(c.allows("bd-2", ts("2026-04-20T12:00:00Z")));
}

/// Items strictly older than the cursor ts are always suppressed.
#[test]
fn suppresses_older_items() {
    let c = cursor_with_ids("2026-04-20T12:00:00Z", &[]);
    assert!(!c.allows("bd-old", ts("2026-04-20T11:59:59Z")));
}

/// Mixing: id-in-boundary suppressed, id-not-in-boundary at same ts passes,
/// strictly newer passes, strictly older suppressed.
#[test]
fn same_timestamp_no_replay() {
    let boundary_ts = "2026-04-20T12:00:00Z";
    // Cursor has seen bd-1 and bd-2 at the boundary timestamp.
    let c = cursor_with_ids(boundary_ts, &["bd-1", "bd-2"]);

    // Replayed items (already seen) — must be suppressed.
    assert!(!c.allows("bd-1", ts(boundary_ts)));
    assert!(!c.allows("bd-2", ts(boundary_ts)));

    // A third issue that arrives with the same timestamp — must pass.
    assert!(c.allows("bd-3", ts(boundary_ts)));

    // Strictly newer — must pass.
    assert!(c.allows("bd-4", ts("2026-04-20T12:00:01Z")));

    // Older — must be suppressed.
    assert!(!c.allows("bd-0", ts("2026-04-20T11:59:59Z")));
}

// ─── JSON round-trip ─────────────────────────────────────────────────────────

#[test]
fn cursor_json_roundtrips() {
    let original = PollCursor {
        ts: ts("2026-04-20T12:00:00Z"),
        ids_at_boundary: ["bd-1", "bd-2"].iter().map(|s| s.to_string()).collect(),
    };

    let json = serde_json::to_string(&original).expect("serialize");
    let decoded: PollCursor = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(decoded.ts, original.ts);
    assert_eq!(decoded.ids_at_boundary, original.ids_at_boundary);
}

#[test]
fn cursor_json_roundtrips_empty_ids() {
    let original = PollCursor {
        ts: ts("2026-04-20T00:00:00Z"),
        ids_at_boundary: HashSet::new(),
    };
    let json = serde_json::to_string(&original).expect("serialize");
    let decoded: PollCursor = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded.ts, original.ts);
    assert!(decoded.ids_at_boundary.is_empty());
}

// ─── Backward-compat: legacy RFC3339 cursor file ─────────────────────────────

/// Load a cursor file that was written by v0a.1 (plain RFC3339 string).
/// Should parse successfully with an empty `ids_at_boundary`.
#[test]
fn cursor_backcompat_parses_legacy_rfc3339() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut f = NamedTempFile::new().unwrap();
    write!(f, "2026-04-20T12:00:00Z").unwrap();
    f.flush().unwrap();

    // Re-implement the same logic as BeadsAdapter::load_cursor for testing
    // (without needing to construct an adapter, which requires `br`).
    let contents = std::fs::read_to_string(f.path()).unwrap();
    let trimmed = contents.trim();

    // JSON parse should fail (it's not JSON).
    assert!(serde_json::from_str::<PollCursor>(trimmed).is_err());

    // RFC3339 parse should succeed.
    let parsed_ts: DateTime<Utc> = trimmed.parse().expect("RFC3339 parse");
    let cursor = PollCursor {
        ts: parsed_ts,
        ids_at_boundary: HashSet::new(),
    };

    assert_eq!(cursor.ts, ts("2026-04-20T12:00:00Z"));
    assert!(cursor.ids_at_boundary.is_empty());
}
