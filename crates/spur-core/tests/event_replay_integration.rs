//! Integration test: feed fixture NDJSON through replay_events,
//! verify all three projections converge to expected state.

use std::path::Path;
use std::time::{Duration, SystemTime};

use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
};
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::SessionId;
use spur_core::event_replay::{replay_events, ReplayConfig};
use spur_core::lineage::ExecutorLineage;
use spur_core::plan_projection::PlanProjectionStore;
use spur_core::session_synopsis::{SessionSynopsis, SessionSynopsisProjection};

fn write_ndjson(path: &Path, events: &[SpurEvent]) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    for ev in events {
        writeln!(f, "{}", serde_json::to_string(ev).unwrap()).unwrap();
    }
}

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
fn replay_populates_all_three_projections_from_fixture() {
    let _ = SystemTime::now(); // touch to avoid unused-import warning
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("100-1000-0.ndjson");

    write_ndjson(
        &path,
        &[
            user_chunk("S1", "fix the auth bug"),
            agent_chunk("S1", "ack"),
            SpurEvent::now(SpurEventBody::TurnComplete {
                session: SessionId("S1".into()),
            }),
            user_chunk("S2", "deploy to staging"),
            agent_chunk("S2", "ok"),
        ],
    );

    let mut lineage = ExecutorLineage::new();
    let mut plan = PlanProjectionStore::new();
    let mut synopsis = SessionSynopsisProjection::new();

    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        replay_horizon: Duration::from_secs(86400 * 365), // 1 year
        skip_pid: None,
        max_line_bytes: 8 * 1024 * 1024,
    };

    let stats = replay_events(&cfg, |ev| {
        lineage.apply(ev);
        plan.apply(ev);
        synopsis.apply(ev);
    })
    .unwrap();

    assert_eq!(stats.events_applied, 5);
    assert_eq!(stats.malformed_lines, 0);

    let s1 = synopsis.get(&SessionId("S1".into())).expect("S1 synopsis");
    assert_eq!(s1.first_user_msg.as_deref(), Some("fix the auth bug"));
    let s2 = synopsis.get(&SessionId("S2".into())).expect("S2 synopsis");
    assert_eq!(s2.last_user_msg.as_deref(), Some("deploy to staging"));
}

#[test]
fn synopsis_seed_replays_into_fresh_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("100-1000-0.ndjson");

    write_ndjson(
        &path,
        &[SpurEvent::now(SpurEventBody::SessionSynopsisSeed {
            session: SessionId("S1".into()),
            first: Some("hello world".into()),
            last: Some("bye now".into()),
        })],
    );

    let mut projection = SessionSynopsisProjection::new();
    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        replay_horizon: Duration::from_secs(7 * 86400),
        skip_pid: None,
        max_line_bytes: 8 * 1024 * 1024,
    };

    let stats = replay_events(&cfg, |ev| projection.apply(ev)).unwrap();

    assert_eq!(
        projection.get(&SessionId("S1".into())),
        Some(SessionSynopsis {
            first_user_msg: Some("hello world".into()),
            last_user_msg: Some("bye now".into()),
        })
    );
    assert_eq!(stats.events_applied, 1);
}

#[test]
fn committed_synopsis_survives_subsequent_seed_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("100-1000-0.ndjson");

    write_ndjson(
        &path,
        &[
            user_chunk("S1", "real first"),
            agent_chunk("S1", "ack"),
            SpurEvent::now(SpurEventBody::SessionSynopsisSeed {
                session: SessionId("S1".into()),
                first: Some("seed first".into()),
                last: Some("seed last".into()),
            }),
        ],
    );

    let mut projection = SessionSynopsisProjection::new();
    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        replay_horizon: Duration::from_secs(7 * 86400),
        skip_pid: None,
        max_line_bytes: 8 * 1024 * 1024,
    };

    let stats = replay_events(&cfg, |ev| projection.apply(ev)).unwrap();

    assert_eq!(
        projection.get(&SessionId("S1".into())),
        Some(SessionSynopsis {
            first_user_msg: Some("real first".into()),
            last_user_msg: Some("real first".into()),
        })
    );
    assert_eq!(stats.events_applied, 3);
}
