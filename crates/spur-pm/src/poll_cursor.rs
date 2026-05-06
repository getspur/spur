//! Boundary-safe poll cursor shared between adapter implementations.
//!
//! A single `DateTime<Utc>` boundary causes "boundary replay": any row whose
//! `updated_at` equals the cursor ts re-emits on every subsequent poll.
//!
//! The fix: track the set of IDs seen at the boundary timestamp. On the next
//! poll a row passes only if:
//!   - `item.updated_at > cursor.ts`   (strictly newer), OR
//!   - `item.updated_at == cursor.ts && !ids_at_boundary.contains(&item.id)`
//!     (same ts but a genuinely new item we haven't returned yet).

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Maximum rows fetched per `poll()` call.
///
/// Chosen to comfortably exceed realistic concurrent-update volume between
/// two poll ticks. If a single poll returns exactly this many rows, the
/// backend may hold additional qualifying rows that were truncated.
pub const POLL_FETCH_LIMIT: usize = 500;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PollCursor {
    pub ts: DateTime<Utc>,
    pub ids_at_boundary: HashSet<String>,
}

impl PollCursor {
    /// Load a cursor from disk.
    ///
    /// The current disk format is JSON-serialized `PollCursor`, matching
    /// `BeadsAdapter`'s persisted cursor. For compatibility with older cursor
    /// files, a bare RFC3339 timestamp is also accepted and upgraded with an
    /// empty `ids_at_boundary` set.
    pub fn load_from(path: &Path) -> anyhow::Result<Option<Self>> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("read cursor file {}", path.display()))
            }
        };

        Self::from_persisted_str(&contents)
            .map(Some)
            .with_context(|| format!("parse cursor file {}", path.display()))
    }

    /// Write the cursor using the same compact JSON serde format as
    /// `BeadsAdapter`.
    ///
    /// Atomic on POSIX: writes to a sibling `.tmp` file then renames over the
    /// target path. A crash mid-write leaves either the previous cursor or
    /// the new one — never a half-written file. The temp file is removed on
    /// any failure path so retries don't accumulate stale `.tmp` debris.
    pub fn write_to(&self, path: &Path) -> anyhow::Result<()> {
        let encoded = serde_json::to_string(self).context("serialize poll cursor")?;
        let tmp_path = match path.file_name() {
            Some(name) => {
                let mut tmp_name = name.to_os_string();
                tmp_name.push(".tmp");
                path.with_file_name(tmp_name)
            }
            None => anyhow::bail!("cursor path has no file name: {}", path.display()),
        };
        if let Err(e) = std::fs::write(&tmp_path, encoded) {
            return Err(e).with_context(|| format!("write cursor tmp file {}", tmp_path.display()));
        }
        if let Err(e) = std::fs::rename(&tmp_path, path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e).with_context(|| {
                format!(
                    "rename cursor tmp {} -> {}",
                    tmp_path.display(),
                    path.display()
                )
            });
        }
        Ok(())
    }

    fn from_persisted_str(contents: &str) -> anyhow::Result<Self> {
        let trimmed = contents.trim();
        if let Ok(cursor) = serde_json::from_str::<Self>(trimmed) {
            return Ok(cursor);
        }

        if let Ok(ts) = trimmed.parse::<DateTime<Utc>>() {
            return Ok(Self {
                ts,
                ids_at_boundary: HashSet::new(),
            });
        }

        anyhow::bail!("cursor file is neither a JSON PollCursor nor an RFC3339 timestamp")
    }

    /// Returns `true` if `item` should be included in the current poll's output.
    pub fn allows(&self, item_id: &str, item_updated_at: DateTime<Utc>) -> bool {
        if item_updated_at > self.ts {
            true
        } else if item_updated_at == self.ts {
            !self.ids_at_boundary.contains(item_id)
        } else {
            false
        }
    }
}
