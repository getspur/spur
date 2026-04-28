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
            SpurEventBody::TurnComplete { session } => {
                self.flush_pending(session);
            }
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
        if s.first_user_msg.is_none() && !trimmed.starts_with('/') {
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

    #[test]
    fn multi_chunk_user_message_accumulates_then_flushes_as_one() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "fix the "));
        proj.apply(&user_chunk_event("S1", "auth bug"));
        proj.apply(&agent_chunk_event("S1", "ack"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("fix the auth bug"));
        assert_eq!(s.last_user_msg.as_deref(), Some("fix the auth bug"));
    }

    #[test]
    fn second_user_message_in_same_session_updates_last_only() {
        let mut proj = SessionSynopsisProjection::new();
        // Turn 1.
        proj.apply(&user_chunk_event("S1", "first request"));
        proj.apply(&agent_chunk_event("S1", "ok"));
        // Turn 2.
        proj.apply(&user_chunk_event("S1", "second request"));
        proj.apply(&agent_chunk_event("S1", "ok"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("first request"));
        assert_eq!(s.last_user_msg.as_deref(), Some("second request"));
    }

    #[test]
    fn slash_command_first_message_does_not_become_first_user_msg() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "/clear"));
        proj.apply(&agent_chunk_event("S1", "ok"));
        proj.apply(&user_chunk_event("S1", "real first message"));
        proj.apply(&agent_chunk_event("S1", "ack"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(
            s.first_user_msg.as_deref(),
            Some("real first message"),
            "slash-command should not lock in as first_user_msg"
        );
        // last_user_msg DOES get the most recent submission, even slash if it's last.
        assert_eq!(s.last_user_msg.as_deref(), Some("real first message"));
    }

    #[test]
    fn slash_command_still_updates_last_user_msg_when_most_recent() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "real msg"));
        proj.apply(&agent_chunk_event("S1", "ok"));
        proj.apply(&user_chunk_event("S1", "/clear"));
        proj.apply(&agent_chunk_event("S1", "ok"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("real msg"));
        assert_eq!(s.last_user_msg.as_deref(), Some("/clear"));
    }

    #[test]
    fn whitespace_only_user_message_does_not_commit_synopsis() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "   \t\n  "));
        proj.apply(&agent_chunk_event("S1", "ok"));

        assert!(
            proj.get(&SessionId("S1".into())).is_none(),
            "whitespace-only flush should not create a synopsis"
        );
    }

    #[test]
    fn empty_chunk_then_real_chunk_commits_only_real_text() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", ""));
        proj.apply(&user_chunk_event("S1", "actual content"));
        proj.apply(&agent_chunk_event("S1", "ok"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("actual content"));
    }

    #[test]
    fn turn_complete_flushes_pending_buffer() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "abandoned partial msg"));
        // No agent reply — only TurnComplete.
        proj.apply(&SpurEvent::now(SpurEventBody::TurnComplete {
            session: SessionId("S1".into()),
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("abandoned partial msg"));
        assert_eq!(s.last_user_msg.as_deref(), Some("abandoned partial msg"));
    }
}
