//! End-to-end: synthetic SpurEvents through App -> projection -> picker label.

use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
};
use spur_acp::domain::events::{HistoryEntry, SpurEvent, SpurEventBody};
use spur_acp::SessionId;
use spur_tui::app::App;

fn user_chunk(session: &str, text: &str) -> SpurEvent {
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

fn agent_chunk(session: &str, text: &str) -> SpurEvent {
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
fn live_user_chunks_produce_synopsis_visible_to_picker() {
    let mut app = App::new(None, true);
    app.handle_spur_event(user_chunk("S1", "fix the auth refactor bug"));
    app.handle_spur_event(agent_chunk("S1", "ack"));

    let s = app
        .synopsis()
        .get(&SessionId("S1".into()))
        .expect("synopsis present after agent reply flushes pending");
    assert_eq!(
        s.first_user_msg.as_deref(),
        Some("fix the auth refactor bug")
    );
    assert_eq!(
        s.last_user_msg.as_deref(),
        Some("fix the auth refactor bug")
    );
}

#[test]
fn session_history_replay_populates_synopsis_for_kiro_path() {
    let mut app = App::new(None, true);
    app.handle_spur_event(SpurEvent::now(SpurEventBody::SessionHistory {
        session: SessionId("kiro1".into()),
        entries: vec![
            HistoryEntry {
                role: "user".into(),
                text: "first kiro".into(),
            },
            HistoryEntry {
                role: "assistant".into(),
                text: "ok".into(),
            },
            HistoryEntry {
                role: "user".into(),
                text: "second kiro".into(),
            },
        ],
    }));

    let s = app
        .synopsis()
        .get(&SessionId("kiro1".into()))
        .expect("synopsis present after kiro history replay");
    assert_eq!(s.first_user_msg.as_deref(), Some("first kiro"));
    assert_eq!(s.last_user_msg.as_deref(), Some("second kiro"));
}
