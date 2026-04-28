//! Session synopsis projection — derived from the event stream.
//!
//! Mirrors `ExecutorLineage` in shape: a passive `apply(&event)` struct
//! that consumers feed from their broadcast subscription. Read API is
//! pure functions over the in-memory state.

use std::collections::HashMap;

use spur_acp::SessionId;

/// First and last user-authored message text for a session, derived
/// from observed events. Stored raw; render-side consumers do their
/// own truncation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionSynopsis {
    pub first_user_msg: Option<String>,
    pub last_user_msg: Option<String>,
}

/// In-memory projection of session synopses, fed by `apply(&event)`.
#[derive(Debug, Default)]
pub struct SessionSynopsisProjection {
    by_session: HashMap<SessionId, SessionSynopsis>,
    #[allow(dead_code)] // Populated by `apply` in Task 2; commit-on-read uses it in Task 9.
    pending: HashMap<SessionId, String>,
}

impl SessionSynopsisProjection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read API. Returns `None` for unknown sessions.
    /// (Commit-on-read fallback added in Task 9.)
    pub fn get(&self, id: &SessionId) -> Option<SessionSynopsis> {
        self.by_session.get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_starts_empty() {
        let proj = SessionSynopsisProjection::new();
        assert!(proj.get(&spur_acp::SessionId("missing".into())).is_none());
    }

    #[test]
    fn synopsis_default_has_no_messages() {
        let s = SessionSynopsis::default();
        assert!(s.first_user_msg.is_none());
        assert!(s.last_user_msg.is_none());
    }
}
