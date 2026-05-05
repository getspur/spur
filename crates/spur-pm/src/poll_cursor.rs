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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PollCursor {
    pub ts: DateTime<Utc>,
    pub ids_at_boundary: HashSet<String>,
}

impl PollCursor {
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
