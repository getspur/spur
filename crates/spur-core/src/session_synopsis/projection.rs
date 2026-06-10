//! Session synopsis projection — derived from the event stream.
//!
//! Mirrors `ExecutorLineage` in shape: a passive `apply(&event)` struct
//! that consumers feed from their broadcast subscription. Read API is
//! pure functions over the in-memory state.

use std::collections::{HashMap, VecDeque};

use spur_acp::SessionId;

/// A single conversation exchange (user turn + optional agent reply)
/// stored in the `recent_exchanges` ring buffer of [`SessionSynopsis`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SynopsisExchange {
    pub user: String,
    pub agent: Option<String>,
}

/// First and last user-authored message text for a session, plus the
/// agent's reply that followed each, derived from observed events.
/// Stored values are capped at write-time so the projection never holds
/// arbitrarily large strings; render-side consumers do their own wrap
/// and per-row truncation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionSynopsis {
    pub first_user_msg: Option<String>,
    pub last_user_msg: Option<String>,
    pub first_agent_reply: Option<String>,
    pub last_agent_reply: Option<String>,
    /// Ring buffer of the last up-to-3 conversation exchanges, in
    /// chronological order (oldest at front, newest at back). Populated
    /// incrementally via `flush_pending` / `flush_pending_agent`, and
    /// rebuilt from authoritative history on `SessionHistory` events.
    pub recent_exchanges: VecDeque<SynopsisExchange>,
}

/// Max stored characters per user-message field.
const USER_MSG_CAP: usize = 200;
/// Max stored characters per agent-reply field.
const AGENT_REPLY_CAP: usize = 100;
/// Maximum number of exchanges kept in the recent-exchanges ring.
const RECENT_EXCHANGES_CAP: usize = 3;
/// Max stored characters per user turn in the recent-exchanges ring.
/// Intentionally smaller than `USER_MSG_CAP` — the ring is optimised for
/// preview rendering, not full-fidelity storage.
const RECENT_USER_CAP: usize = 100;
/// Max stored characters per agent turn in the recent-exchanges ring.
/// Intentionally larger than `AGENT_REPLY_CAP` — agent previews benefit
/// from slightly more context than the legacy first/last fields.
const RECENT_AGENT_CAP: usize = 200;

/// In-memory projection of session synopses, fed by `apply(&event)`.
#[derive(Debug, Default)]
pub struct SessionSynopsisProjection {
    by_session: HashMap<SessionId, SessionSynopsis>,
    pending: HashMap<SessionId, String>,
    pending_agent: HashMap<SessionId, String>,
}

fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max).collect()
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
                first_user_msg: if is_slash_command_like(p) {
                    None
                } else {
                    Some(cap_chars(p, USER_MSG_CAP))
                },
                last_user_msg: Some(cap_chars(p, USER_MSG_CAP)),
                first_agent_reply: None,
                last_agent_reply: None,
                recent_exchanges: VecDeque::new(),
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
    ///
    /// **Not idempotent under double-apply on `AgentNotification(UserMessageChunk)`**:
    /// the chunk text is appended to the pending buffer, so re-applying the same
    /// chunk doubles the buffer. The replay model in
    /// `crates/spur-core/src/event_replay.rs` is structurally guarded against
    /// double-apply via PID-filtered file selection.
    pub fn apply(&mut self, event: &spur_acp::SpurEvent) {
        use agent_client_protocol::schema::SessionUpdate;
        use spur_acp::domain::events::SpurEventBody;

        match &event.body {
            SpurEventBody::AgentNotification {
                session,
                notification,
            } => match &notification.update {
                SessionUpdate::UserMessageChunk(chunk) => {
                    // A new user turn closes out any in-flight agent reply.
                    self.flush_pending_agent(session);
                    let text = content_block_text(&chunk.content);
                    self.pending
                        .entry(session.clone())
                        .or_default()
                        .push_str(text);
                }
                SessionUpdate::AgentMessageChunk(chunk) => {
                    // Agent turn started — finalize any pending user buffer
                    // first, then accumulate the agent reply text.
                    self.flush_pending(session);
                    let text = content_block_text(&chunk.content);
                    self.pending_agent
                        .entry(session.clone())
                        .or_default()
                        .push_str(text);
                }
                // Non-text agent updates still close out a pending user buffer
                // but do NOT flush the agent reply — agent text can resume
                // streaming after a thought/tool-call within the same turn.
                SessionUpdate::AgentThoughtChunk(_)
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
                self.flush_pending_agent(session);
            }
            SpurEventBody::BrainRetired { session, .. }
            | SpurEventBody::SessionCompleted { session, .. } => {
                self.flush_pending(session);
                self.flush_pending_agent(session);
            }
            SpurEventBody::SessionAttachRejected { acp_session_id, .. } => {
                let key = SessionId(acp_session_id.clone());
                self.flush_pending(&key);
                self.flush_pending_agent(&key);
            }
            SpurEventBody::SessionHistory { session, entries } => {
                // Drop any stale pending buffers for this session — history is
                // authoritative.
                self.pending.remove(session);
                self.pending_agent.remove(session);

                // Collect all non-empty user entry indices in document order.
                let user_indices: Vec<usize> = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.role == "user" && !e.text.trim().is_empty())
                    .map(|(i, _)| i)
                    .collect();

                if user_indices.is_empty() {
                    return;
                }

                let next_assistant_text = |after_idx: usize| -> Option<&str> {
                    entries
                        .iter()
                        .skip(after_idx + 1)
                        .find(|e| e.role == "assistant")
                        .map(|e| e.text.trim())
                        .filter(|t| !t.is_empty())
                };

                // Determine first non-slash user index and last user index.
                let first_user_idx = user_indices
                    .iter()
                    .copied()
                    .find(|&i| !is_slash_command_like(entries[i].text.trim()));
                let last_user_idx = *user_indices.last().unwrap(); // always present

                let s = self.by_session.entry(session.clone()).or_default();
                if let Some(idx) = first_user_idx {
                    if s.first_user_msg.is_none() {
                        s.first_user_msg = Some(cap_chars(entries[idx].text.trim(), USER_MSG_CAP));
                    }
                    if s.first_agent_reply.is_none() {
                        if let Some(reply) = next_assistant_text(idx) {
                            s.first_agent_reply = Some(cap_chars(reply, AGENT_REPLY_CAP));
                        }
                    }
                }
                s.last_user_msg = Some(cap_chars(entries[last_user_idx].text.trim(), USER_MSG_CAP));
                if let Some(reply) = next_assistant_text(last_user_idx) {
                    s.last_agent_reply = Some(cap_chars(reply, AGENT_REPLY_CAP));
                }

                // Rebuild the recent-exchanges ring from the last up-to-3
                // non-empty user entries (chronological order, oldest first).
                // History is authoritative — always replace any prior ring.
                let recent_start = user_indices.len().saturating_sub(RECENT_EXCHANGES_CAP);
                s.recent_exchanges = user_indices[recent_start..]
                    .iter()
                    .map(|&i| SynopsisExchange {
                        user: cap_chars(entries[i].text.trim(), RECENT_USER_CAP),
                        agent: next_assistant_text(i).map(|r| cap_chars(r, RECENT_AGENT_CAP)),
                    })
                    .collect();
            }
            SpurEventBody::SessionSynopsisSeed {
                session,
                first,
                last,
            } => {
                if self.by_session.contains_key(session) {
                    return;
                }

                self.pending.remove(session);
                self.pending_agent.remove(session);
                let synopsis = SessionSynopsis {
                    first_user_msg: first
                        .clone()
                        .filter(|s| !is_slash_command_like(s))
                        .map(|s| cap_chars(&s, USER_MSG_CAP)),
                    last_user_msg: last.clone().map(|s| cap_chars(&s, USER_MSG_CAP)),
                    first_agent_reply: None,
                    last_agent_reply: None,
                    recent_exchanges: VecDeque::new(),
                };

                if synopsis.first_user_msg.is_none() && synopsis.last_user_msg.is_none() {
                    return;
                }

                self.by_session.insert(session.clone(), synopsis);
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
        let capped = cap_chars(trimmed, USER_MSG_CAP);
        let s = self.by_session.entry(session.clone()).or_default();
        if s.first_user_msg.is_none() && !is_slash_command_like(trimmed) {
            s.first_user_msg = Some(capped.clone());
        }
        s.last_user_msg = Some(capped);

        // Ring entries must mirror last_user_msg semantics exactly: every
        // flushed non-empty user turn enters the ring, slash commands
        // included; whitespace-only buffers were filtered above.
        let exchange = SynopsisExchange {
            user: cap_chars(trimmed, RECENT_USER_CAP),
            agent: None,
        };
        s.recent_exchanges.push_back(exchange);
        if s.recent_exchanges.len() > RECENT_EXCHANGES_CAP {
            s.recent_exchanges.pop_front();
        }
    }

    fn flush_pending_agent(&mut self, session: &SessionId) {
        let buf = match self.pending_agent.remove(session) {
            Some(b) => b,
            None => return,
        };
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            return;
        }
        let capped = cap_chars(trimmed, AGENT_REPLY_CAP);
        // Attribution requires SOME committed user message. first_agent_reply
        // additionally requires a real (non-slash) first_user_msg (inner guard
        // below); last_agent_reply and the ring slot are filled whenever any
        // user message is committed, including slash-command-only turns.
        let s = match self.by_session.get_mut(session) {
            Some(s) if s.first_user_msg.is_some() || s.last_user_msg.is_some() => s,
            _ => return,
        };
        if s.first_user_msg.is_some() && s.first_agent_reply.is_none() {
            s.first_agent_reply = Some(capped.clone());
        }
        s.last_agent_reply = Some(capped);

        // Fill the agent slot of the newest (back) exchange, but only when it
        // is still unfilled. Never overwrite an already-committed agent reply in
        // the ring — the ring entry records the reply for that specific user turn.
        let reply_capped = cap_chars(trimmed, RECENT_AGENT_CAP);
        if let Some(newest) = s.recent_exchanges.back_mut() {
            if newest.agent.is_none() {
                newest.agent = Some(reply_capped);
            }
        }
    }
}

/// Returns true if `text` is a slash-command submission or a Claude Code
/// slash-command wrapper (`<command-name>/foo</command-name>...`). These
/// should be skipped when picking a session's `first_user_msg` so the
/// rendered "Intent" reflects the real first request, not control input.
fn is_slash_command_like(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('/') || t.starts_with("<command-name>")
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

    fn seed_event(session: &str, first: Option<&str>, last: Option<&str>) -> SpurEvent {
        SpurEvent::now(SpurEventBody::SessionSynopsisSeed {
            session: SessionId(session.into()),
            first: first.map(str::to_owned),
            last: last.map(str::to_owned),
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
    fn session_history_skips_leading_slash_command_for_first_user_msg() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![
                history_entry("user", "/clear"),
                history_entry("assistant", "ok"),
                history_entry("user", "real first message"),
                history_entry("assistant", "ack"),
                history_entry("user", "later message"),
            ],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(
            s.first_user_msg.as_deref(),
            Some("real first message"),
            "first_user_msg should skip leading slash-command"
        );
        assert_eq!(s.last_user_msg.as_deref(), Some("later message"));
    }

    #[test]
    fn session_history_all_slash_commands_leaves_first_user_msg_none() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![
                history_entry("user", "/clear"),
                history_entry("assistant", "ok"),
                history_entry("user", "/help"),
            ],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert!(
            s.first_user_msg.is_none(),
            "all-slash history has no real first"
        );
        assert_eq!(s.last_user_msg.as_deref(), Some("/help"));
    }

    #[test]
    fn seed_populates_empty_projection() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&seed_event("S1", Some("hello"), Some("bye")));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("hello"));
        assert_eq!(s.last_user_msg.as_deref(), Some("bye"));
    }

    #[test]
    fn seed_does_not_overwrite_committed() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "real"));
        proj.apply(&agent_chunk_event("S1", "ok"));
        proj.apply(&seed_event("S1", Some("seed first"), Some("seed last")));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("real"));
        assert_eq!(s.last_user_msg.as_deref(), Some("real"));
    }

    #[test]
    fn seed_skips_slash_command_for_first_user_msg() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&seed_event("S1", Some("/clear"), Some("/clear")));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert!(s.first_user_msg.is_none());
        assert_eq!(s.last_user_msg.as_deref(), Some("/clear"));
    }

    #[test]
    fn seed_with_both_none_is_noop() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&seed_event("S1", None, None));

        assert!(proj.get(&SessionId("S1".into())).is_none());
    }

    #[test]
    fn seed_clears_stale_pending() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "stale partial"));
        proj.apply(&seed_event("S1", Some("seed first"), Some("seed last")));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("seed first"));
        assert_eq!(s.last_user_msg.as_deref(), Some("seed last"));
        assert!(!proj.pending.contains_key(&SessionId("S1".into())));
    }

    #[test]
    fn duplicate_seeds_are_idempotent() {
        let mut once = SessionSynopsisProjection::new();
        once.apply(&seed_event("S1", Some("hello"), Some("bye")));

        let mut twice = SessionSynopsisProjection::new();
        let event = seed_event("S1", Some("hello"), Some("bye"));
        twice.apply(&event);
        twice.apply(&event);

        assert_eq!(
            twice.get(&SessionId("S1".into())),
            once.get(&SessionId("S1".into()))
        );
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
    fn first_agent_reply_captured_after_first_user_turn_completes() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "fix the auth bug"));
        proj.apply(&agent_chunk_event("S1", "I'll take a look."));
        // Second user turn triggers the flush of the pending agent buffer.
        proj.apply(&user_chunk_event("S1", "thanks"));
        proj.apply(&agent_chunk_event("S1", "done"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("fix the auth bug"));
        assert_eq!(s.first_agent_reply.as_deref(), Some("I'll take a look."));
        assert_eq!(s.last_user_msg.as_deref(), Some("thanks"));
        // last_agent_reply for "done" only commits at TurnComplete / next turn.
        assert_eq!(s.last_agent_reply.as_deref(), Some("I'll take a look."));

        proj.apply(&SpurEvent::now(SpurEventBody::TurnComplete {
            session: SessionId("S1".into()),
        }));
        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.last_agent_reply.as_deref(), Some("done"));
        // first_agent_reply must not be overwritten by later turns.
        assert_eq!(s.first_agent_reply.as_deref(), Some("I'll take a look."));
    }

    #[test]
    fn multi_chunk_agent_reply_accumulates_then_flushes_as_one() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "hi"));
        proj.apply(&agent_chunk_event("S1", "hello "));
        proj.apply(&agent_chunk_event("S1", "there"));
        proj.apply(&SpurEvent::now(SpurEventBody::TurnComplete {
            session: SessionId("S1".into()),
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_agent_reply.as_deref(), Some("hello there"));
        assert_eq!(s.last_agent_reply.as_deref(), Some("hello there"));
    }

    #[test]
    fn agent_thought_or_tool_call_does_not_split_agent_reply() {
        use agent_client_protocol::schema::{ContentChunk, TextContent};
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "do thing"));
        proj.apply(&agent_chunk_event("S1", "step 1 "));
        // A thought chunk arrives mid-turn — must not finalize the reply.
        proj.apply(&SpurEvent::now(SpurEventBody::AgentNotification {
            session: SessionId("S1".into()),
            notification: Box::new(SessionNotification::new(
                agent_client_protocol::schema::SessionId::new("S1"),
                SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new("thinking..."),
                ))),
            )),
        }));
        proj.apply(&agent_chunk_event("S1", "step 2"));
        proj.apply(&SpurEvent::now(SpurEventBody::TurnComplete {
            session: SessionId("S1".into()),
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_agent_reply.as_deref(), Some("step 1 step 2"));
    }

    #[test]
    fn agent_reply_caps_at_100_chars() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "go"));
        proj.apply(&agent_chunk_event("S1", &"x".repeat(250)));
        proj.apply(&SpurEvent::now(SpurEventBody::TurnComplete {
            session: SessionId("S1".into()),
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_agent_reply.as_deref().map(str::len), Some(100));
    }

    #[test]
    fn user_msg_caps_at_200_chars() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", &"a".repeat(500)));
        proj.apply(&agent_chunk_event("S1", "ok"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref().map(str::len), Some(200));
        assert_eq!(s.last_user_msg.as_deref().map(str::len), Some(200));
    }

    #[test]
    fn agent_reply_not_attributed_when_only_slash_command_user_history() {
        // Slash-only user submissions should not lock in a first_agent_reply.
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "/clear"));
        proj.apply(&agent_chunk_event("S1", "cleared"));
        proj.apply(&SpurEvent::now(SpurEventBody::TurnComplete {
            session: SessionId("S1".into()),
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert!(s.first_user_msg.is_none());
        assert!(
            s.first_agent_reply.is_none(),
            "agent reply must not lock in without a real first_user_msg"
        );
    }

    #[test]
    fn session_history_captures_agent_replies_for_first_and_last() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![
                history_entry("user", "/clear"),
                history_entry("assistant", "cleared"),
                history_entry("user", "real first"),
                history_entry("assistant", "first reply"),
                history_entry("user", "second"),
                history_entry("assistant", "second reply"),
                history_entry("user", "third"),
                history_entry("assistant", "third reply"),
            ],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("real first"));
        assert_eq!(s.first_agent_reply.as_deref(), Some("first reply"));
        assert_eq!(s.last_user_msg.as_deref(), Some("third"));
        assert_eq!(s.last_agent_reply.as_deref(), Some("third reply"));
    }

    #[test]
    fn session_history_handles_missing_trailing_assistant_reply() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![
                history_entry("user", "hello"),
                history_entry("assistant", "hi back"),
                history_entry("user", "follow up"),
                // No assistant reply after the last user entry.
            ],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_agent_reply.as_deref(), Some("hi back"));
        // last_agent_reply falls back to the only assistant entry, which is
        // also the next-after-last-user lookup result (none after "follow up").
        // We treat the missing trailing reply as "none" rather than reusing
        // the first reply — keeps the semantics honest.
        assert!(s.last_agent_reply.is_none());
    }

    #[test]
    fn claude_code_command_wrapper_is_treated_as_slash_command() {
        // Claude Code wraps slash commands like `/model` into a structured
        // user message starting with `<command-name>/model</command-name>...`.
        // That wrapper text should not become the session's first_user_msg.
        let wrapper = "<command-name>/model</command-name>\n            \
                       <command-message>model</command-message>\n            \
                       <command-args>claude-opus-4-7[1m]</command-args>";
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", wrapper));
        proj.apply(&agent_chunk_event("S1", "ok"));
        proj.apply(&user_chunk_event("S1", "real first message"));
        proj.apply(&agent_chunk_event("S1", "ack"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("real first message"));

        // Same protection via SessionHistory replay.
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S2".into()),
            entries: vec![
                history_entry("user", wrapper),
                history_entry("assistant", "ok"),
                history_entry("user", "real first via history"),
            ],
        }));
        let s = proj.get(&SessionId("S2".into())).unwrap();
        assert_eq!(s.first_user_msg.as_deref(), Some("real first via history"));

        // And via SessionSynopsisSeed.
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&seed_event("S3", Some(wrapper), Some(wrapper)));
        let s = proj.get(&SessionId("S3".into())).unwrap();
        assert!(s.first_user_msg.is_none());
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

    // ── recent_exchanges ring buffer tests ────────────────────────────────────

    fn turn_complete(session: &str) -> SpurEvent {
        SpurEvent::now(SpurEventBody::TurnComplete {
            session: SessionId(session.into()),
        })
    }

    /// Drive a complete turn: user msg → agent reply → TurnComplete.
    fn do_turn(proj: &mut SessionSynopsisProjection, session: &str, user: &str, agent: &str) {
        proj.apply(&user_chunk_event(session, user));
        proj.apply(&agent_chunk_event(session, agent));
        proj.apply(&turn_complete(session));
    }

    #[test]
    fn ring_captures_exchanges_in_order_and_evicts_oldest_on_fourth() {
        let mut proj = SessionSynopsisProjection::new();
        do_turn(&mut proj, "S1", "msg1", "reply1");
        do_turn(&mut proj, "S1", "msg2", "reply2");
        do_turn(&mut proj, "S1", "msg3", "reply3");

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.recent_exchanges.len(), 3);
        assert_eq!(s.recent_exchanges[0].user, "msg1");
        assert_eq!(s.recent_exchanges[0].agent.as_deref(), Some("reply1"));
        assert_eq!(s.recent_exchanges[1].user, "msg2");
        assert_eq!(s.recent_exchanges[1].agent.as_deref(), Some("reply2"));
        assert_eq!(s.recent_exchanges[2].user, "msg3");
        assert_eq!(s.recent_exchanges[2].agent.as_deref(), Some("reply3"));

        // Fourth turn evicts the oldest.
        do_turn(&mut proj, "S1", "msg4", "reply4");
        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.recent_exchanges.len(), 3);
        assert_eq!(s.recent_exchanges[0].user, "msg2", "oldest evicted");
        assert_eq!(s.recent_exchanges[1].user, "msg3");
        assert_eq!(s.recent_exchanges[2].user, "msg4");
    }

    #[test]
    fn ring_caps_user_at_100_and_agent_at_200() {
        let mut proj = SessionSynopsisProjection::new();
        // Use lengths that exceed both the ring caps AND the first/last caps
        // so every truncation point is exercised.
        let long_user = "u".repeat(300); // > USER_MSG_CAP(200) and > RECENT_USER_CAP(100)
        let long_agent = "a".repeat(300); // > AGENT_REPLY_CAP(100) and > RECENT_AGENT_CAP(200)
        proj.apply(&user_chunk_event("S1", &long_user));
        proj.apply(&agent_chunk_event("S1", &long_agent));
        proj.apply(&turn_complete("S1"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        let ex = &s.recent_exchanges[0];
        assert_eq!(
            ex.user.chars().count(),
            100,
            "user capped at RECENT_USER_CAP"
        );
        assert_eq!(
            ex.agent.as_ref().map(|a| a.chars().count()),
            Some(200),
            "agent capped at RECENT_AGENT_CAP"
        );
        // Existing first/last fields should keep their own (larger) caps.
        assert_eq!(
            s.first_user_msg.as_ref().map(|s| s.chars().count()),
            Some(200),
            "first_user_msg capped at USER_MSG_CAP"
        );
        assert_eq!(
            s.first_agent_reply.as_ref().map(|s| s.chars().count()),
            Some(100),
            "first_agent_reply capped at AGENT_REPLY_CAP"
        );
    }

    #[test]
    fn agent_reply_fills_only_newest_exchange_and_does_not_overwrite_already_filled() {
        let mut proj = SessionSynopsisProjection::new();
        // Turn 1 — fully committed with an agent reply.
        do_turn(&mut proj, "S1", "first", "first_reply");
        // Turn 2 user — no agent reply yet.
        proj.apply(&user_chunk_event("S1", "second"));
        proj.apply(&agent_chunk_event("S1", "second_reply"));
        proj.apply(&turn_complete("S1"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        // The first exchange must not have its agent slot changed.
        assert_eq!(
            s.recent_exchanges[0].agent.as_deref(),
            Some("first_reply"),
            "already-filled slot must not be overwritten"
        );
        assert_eq!(s.recent_exchanges[1].agent.as_deref(), Some("second_reply"));
    }

    #[test]
    fn turn_without_agent_reply_yields_exchange_with_agent_none() {
        let mut proj = SessionSynopsisProjection::new();
        // User sends a message; TurnComplete fires with no agent reply.
        proj.apply(&user_chunk_event("S1", "standalone"));
        proj.apply(&turn_complete("S1"));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.recent_exchanges.len(), 1);
        assert_eq!(s.recent_exchanges[0].user, "standalone");
        assert!(
            s.recent_exchanges[0].agent.is_none(),
            "no agent reply → exchange.agent should be None"
        );
    }

    #[test]
    fn session_history_rebuilds_ring_from_last_3_pairs() {
        let mut proj = SessionSynopsisProjection::new();
        // Prime the ring with stale data from live events to verify replace.
        do_turn(&mut proj, "S1", "stale", "stale_reply");

        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![
                history_entry("user", "h1"),
                history_entry("assistant", "a1"),
                history_entry("user", "h2"),
                history_entry("assistant", "a2"),
                history_entry("user", "h3"),
                history_entry("assistant", "a3"),
                history_entry("user", "h4"),
                history_entry("assistant", "a4"),
            ],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        // Ring should hold h2, h3, h4 (last 3), stale data gone.
        assert_eq!(s.recent_exchanges.len(), 3);
        assert_eq!(s.recent_exchanges[0].user, "h2");
        assert_eq!(s.recent_exchanges[0].agent.as_deref(), Some("a2"));
        assert_eq!(s.recent_exchanges[1].user, "h3");
        assert_eq!(s.recent_exchanges[1].agent.as_deref(), Some("a3"));
        assert_eq!(s.recent_exchanges[2].user, "h4");
        assert_eq!(s.recent_exchanges[2].agent.as_deref(), Some("a4"));
    }

    #[test]
    fn session_history_trailing_user_without_assistant_gets_agent_none() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
            session: SessionId("S1".into()),
            entries: vec![
                history_entry("user", "h1"),
                history_entry("assistant", "a1"),
                history_entry("user", "h2"),
                // No assistant after h2.
            ],
        }));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.recent_exchanges.len(), 2);
        assert_eq!(s.recent_exchanges[0].user, "h1");
        assert_eq!(s.recent_exchanges[0].agent.as_deref(), Some("a1"));
        assert_eq!(s.recent_exchanges[1].user, "h2");
        assert!(
            s.recent_exchanges[1].agent.is_none(),
            "trailing user without assistant → agent None"
        );
    }

    #[test]
    fn seed_event_leaves_ring_empty() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&seed_event("S1", Some("hello"), Some("bye")));

        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert!(
            s.recent_exchanges.is_empty(),
            "seed events carry no exchange data"
        );
    }

    #[test]
    fn whitespace_only_flush_pushes_nothing_to_ring() {
        let mut proj = SessionSynopsisProjection::new();
        proj.apply(&user_chunk_event("S1", "   \n  "));
        proj.apply(&agent_chunk_event("S1", "ok"));
        // The whitespace-only buffer was discarded, so by_session has no entry,
        // and no exchange should have been pushed.
        assert!(
            proj.get(&SessionId("S1".into())).is_none(),
            "whitespace-only flush should not create synopsis or exchange"
        );
    }

    #[test]
    fn ring_not_mutated_per_chunk_only_on_flush() {
        let mut proj = SessionSynopsisProjection::new();
        // Multiple chunks of a single user turn: the ring must stay untouched
        // until the turn is flushed, or double-applied chunks would corrupt it.
        proj.apply(&user_chunk_event("S1", "chunk "));
        proj.apply(&user_chunk_event("S1", "one"));
        proj.apply(&user_chunk_event("S1", " more"));
        assert!(
            !proj.by_session.contains_key(&SessionId("S1".into())),
            "no commit (and so no ring write) may happen per-chunk"
        );
        // Agent reply triggers the flush; exactly one exchange appears.
        proj.apply(&agent_chunk_event("S1", "ack"));
        let s = proj.get(&SessionId("S1".into())).unwrap();
        assert_eq!(s.recent_exchanges.len(), 1);
        assert_eq!(s.recent_exchanges[0].user, "chunk one more");
    }
}
