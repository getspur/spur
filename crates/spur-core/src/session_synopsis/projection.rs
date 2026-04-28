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

    /// Read API. Returns the committed synopsis when present. If a
    /// session has only a pending buffer (no committed last_user_msg
    /// yet — abandoned mid-user-turn), exposes the pending text as
    /// last_user_msg and (when not a slash-command) as first_user_msg.
    pub fn get(&self, id: &SessionId) -> Option<SessionSynopsis> {
        let committed = self.by_session.get(id);
        let pending_trimmed = self
            .pending
            .get(id)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        match (committed, pending_trimmed) {
            (Some(c), _) => Some(c.clone()),
            (None, Some(p)) => Some(SessionSynopsis {
                first_user_msg: if p.starts_with('/') {
                    None
                } else {
                    Some(p.to_owned())
                },
                last_user_msg: Some(p.to_owned()),
            }),
            (None, None) => None,
        }
    }

    /// Test-only helper to inject a synopsis directly without going
    /// through the event stream. Visible to in-crate `#[cfg(test)]`
    /// modules and to consumers via the optional `test-helpers` feature.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn insert_for_test(&mut self, id: spur_acp::SessionId, synopsis: SessionSynopsis) {
        self.pending.remove(&id);
        self.by_session.insert(id, synopsis);
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
            SpurEventBody::BrainRetired { session, .. }
            | SpurEventBody::SessionCompleted { session, .. } => {
                self.flush_pending(session);
            }
            SpurEventBody::SessionAttachRejected { acp_session_id, .. } => {
                self.flush_pending(&SessionId(acp_session_id.clone()));
            }
            SpurEventBody::SessionHistory { session, entries } => {
                // Drop any stale pending buffer for this session — the history
                // is authoritative.
                self.pending.remove(session);

                let user_texts: Vec<&str> = entries
                    .iter()
                    .filter(|e| e.role == "user")
                    .map(|e| e.text.trim())
                    .filter(|t| !t.is_empty())
                    .collect();

                if user_texts.is_empty() {
                    return;
                }

                let first = user_texts.first().copied().unwrap();
                let last = user_texts.last().copied().unwrap();
                let s = self.by_session.entry(session.clone()).or_default();
                if s.first_user_msg.is_none() && !first.starts_with('/') {
                    s.first_user_msg = Some(first.to_owned());
                }
                s.last_user_msg = Some(last.to_owned());
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
    use spur_acp::domain::events::{HistoryEntry, SpurEvent, SpurEventBody};

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

    fn history_entry(role: &str, text: &str) -> HistoryEntry {
        HistoryEntry {
            role: role.into(),
            text: text.into(),
        }
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

        // Pending — surfaced via commit-on-read fallback (Task 9), but not yet committed.
        let pending = proj
            .get(&SessionId("S1".into()))
            .expect("commit-on-read exposes pending");
        assert_eq!(pending.last_user_msg.as_deref(), Some("fix the auth bug"));
        assert!(!proj.by_session.contains_key(&SessionId("S1".into())));

        // Agent reply triggers flush.
        proj.apply(&agent_chunk_event("S1", "I'll take a look."));

        let s = proj.get(&SessionId("S1".into())).expect("synopsis present");
        assert_eq!(s.first_user_msg.as_deref(), Some("fix the auth bug"));
        assert_eq!(s.last_user_msg.as_deref(), Some("fix the auth bug"));
        assert!(proj.by_session.contains_key(&SessionId("S1".into())));
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

    #[test]
    fn brain_retired_flushes_pending() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "before retire"));
        proj.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
            session: SessionId("S1".into()),
            reason: spur_acp::domain::events::BrainRetireReason::Shutdown,
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.last_user_msg.as_deref(), Some("before retire"));
    }

    #[test]
    fn session_completed_flushes_pending() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "before complete"));
        proj.apply(&SpurEvent::now(SpurEventBody::SessionCompleted {
            session: SessionId("S1".into()),
            success: true,
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.last_user_msg.as_deref(), Some("before complete"));
    }

    #[test]
    fn session_history_populates_first_and_last_user_msg() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![
                history_entry("user", "first kiro msg"),
                history_entry("assistant", "ack"),
                history_entry("user", "second kiro msg"),
                history_entry("assistant", "ack"),
                history_entry("user", "third kiro msg"),
            ],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("first kiro msg"));
        assert_eq!(s.last_user_msg.as_deref(), Some("third kiro msg"));
    }

    #[test]
    fn session_history_drops_pending_buffer() {
        let mut proj = SessionSynopsisProjection::new();
        // Stale pending from before history arrives.
        proj.apply(&user_chunk_event("S1", "stale partial"));
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![history_entry("user", "real first")],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("real first"));
        // Stale pending should have been dropped, not appended.
        assert_eq!(s.last_user_msg.as_deref(), Some("real first"));
    }

    #[test]
    fn session_history_with_no_user_entries_is_noop() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![history_entry("assistant", "only assistant")],
        }));

        assert!(proj.get(&SessionId("S1".into())).is_none());
    }

    #[test]
    fn session_history_empty_entries_is_noop() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![],
        }));

        assert!(proj.get(&SessionId("S1".into())).is_none());
    }

    #[test]
    fn get_exposes_pending_buffer_when_no_committed_last_msg() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "abandoned mid turn"));

        let s = proj
            .get(&SessionId("S1".into()))
            .expect("commit-on-read should surface pending");
        assert_eq!(s.last_user_msg.as_deref(), Some("abandoned mid turn"));
        assert_eq!(s.first_user_msg.as_deref(), Some("abandoned mid turn"));
    }

    #[test]
    fn get_does_not_promote_slash_command_to_first_user_msg_via_read_fallback() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "/clear"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert!(
            s.first_user_msg.is_none(),
            "slash should not become first via read fallback"
        );
        assert_eq!(s.last_user_msg.as_deref(), Some("/clear"));
    }

    #[test]
    fn get_committed_synopsis_preferred_over_pending() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "committed msg"));
        proj.apply(&agent_chunk_event("S1", "ok"));
        proj.apply(&user_chunk_event("S1", "in-flight new turn"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("committed msg"));
        assert_eq!(s.last_user_msg.as_deref(), Some("committed msg"));
    }

    #[test]
    fn unrelated_event_variants_are_ignored() {
        let mut proj = SessionSynopsisProjection::new();
        // CostUpdate has no session synopsis relevance.
        proj.apply(&SpurEvent::now(SpurEventBody::CostUpdate {
            session: SessionId("S1".into()),
            agent: "claude".into(),
            estimated_cost_usd: 0.001,
        }));
        assert!(proj.get(&SessionId("S1".into())).is_none());
        assert!(proj.get(&SessionId("missing".into())).is_none());
    }
}
