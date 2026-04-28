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

    /// Fold an event into the projection. Idempotent on irrelevant variants.
    // Outer `match` has a single arm for now; later tasks add more event variants.
    #[allow(clippy::single_match)]
    pub fn apply(&mut self, event: &spur_acp::SpurEvent) {
        use agent_client_protocol::schema::SessionUpdate;
        use spur_acp::domain::events::SpurEventBody;

        match &event.body {
            SpurEventBody::AgentNotification {
                session,
                notification,
            } => match &notification.update {
                SessionUpdate::UserMessageChunk(chunk) => {
                    let text = content_block_text(&chunk.content);
                    self.pending
                        .entry(session.clone())
                        .or_default()
                        .push_str(text);
                }
                // Any non-user agent update flushes the pending buffer.
                SessionUpdate::AgentMessageChunk(_)
                | SessionUpdate::AgentThoughtChunk(_)
                | SessionUpdate::ToolCall(_)
                | SessionUpdate::ToolCallUpdate(_)
                | SessionUpdate::Plan(_)
                | SessionUpdate::AvailableCommandsUpdate(_)
                | SessionUpdate::CurrentModeUpdate(_) => {
                    self.flush_pending(session);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn flush_pending(&mut self, session: &SessionId) {
        let buf = match self.pending.remove(session) {
            Some(b) => b,
            None => return,
        };
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            return;
        }
        let s = self.by_session.entry(session.clone()).or_default();
        if s.first_user_msg.is_none() {
            s.first_user_msg = Some(trimmed.to_owned());
        }
        s.last_user_msg = Some(trimmed.to_owned());
    }
}

fn content_block_text(content: &agent_client_protocol::schema::ContentBlock) -> &str {
    use agent_client_protocol::schema::ContentBlock;
    match content {
        ContentBlock::Text(t) => &t.text,
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
    };
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};

    fn user_chunk_event(session: &str, text: &str) -> SpurEvent {
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: SessionId(session.into()),
            notification: Box::new(SessionNotification::new(
                agent_client_protocol::schema::SessionId::new(session),
                SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new(text),
                ))),
            )),
        })
    }

    fn agent_chunk_event(session: &str, text: &str) -> SpurEvent {
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: SessionId(session.into()),
            notification: Box::new(SessionNotification::new(
                agent_client_protocol::schema::SessionId::new(session),
                SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new(text),
                ))),
            )),
        })
    }

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

    #[test]
    fn first_user_chunk_is_buffered_then_flushed_on_agent_reply() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "fix the auth bug"));

        // Pending — not yet committed.
        assert!(proj.get(&SessionId("S1".into())).is_none());

        // Agent reply triggers flush.
        proj.apply(&agent_chunk_event("S1", "I'll take a look."));

        let s = proj.get(&SessionId("S1".into())).expect("synopsis present");
        assert_eq!(s.first_user_msg.as_deref(), Some("fix the auth bug"));
        assert_eq!(s.last_user_msg.as_deref(), Some("fix the auth bug"));
    }
}
