use chrono::Utc;
use proptest::prelude::*;
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource, DelegationKey};
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::ContinuationEventSink;
use spur_core::scheduler::{BrainScheduler, ScheduledAction};
use spur_core::InteractiveInput;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
enum Event {
    PushUser,
    PushContinuation(String),
    TurnStart,
    TurnEnd,
    CancelResolve,
    Tick(u64), // advance clock by N ms
}

fn event_strategy() -> impl Strategy<Value = Event> {
    prop_oneof![
        Just(Event::PushUser),
        "id-[0-9]{1,3}".prop_map(Event::PushContinuation),
        Just(Event::TurnStart),
        Just(Event::TurnEnd),
        Just(Event::CancelResolve),
        (0u64..2000).prop_map(Event::Tick),
    ]
}

fn mk_cont(id: &str) -> BrainContinuation {
    BrainContinuation {
        delegation_id: id.into(),
        attempt: 1,
        brain_session: SessionId("brain-session-1".into()),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: None,
            diff_summary: None,
            worker_branch: None,
            artifact_ref: None,
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
            setup_conflict_topology: None,
        },
        created_at_wall: Utc::now(),
        created_at_mono: Instant::now(),
    }
}

struct NoopSink;

impl ContinuationEventSink for NoopSink {
    fn emit(&self, _body: SpurEventBody) {}
}

proptest! {
    #[test]
    fn no_continuation_is_ever_scheduled_twice(events in prop::collection::vec(event_strategy(), 0..100)) {
        let mut s = BrainScheduler::new(Some(SessionId::new().into()), Arc::new(NoopSink));
        let mut now = Instant::now();
        let mut seen_scheduled_ids: std::collections::HashSet<DelegationKey> = Default::default();

        for e in events {
            match e {
                Event::PushUser => s.push_user(InteractiveInput::Message {
                    blocks: vec![], interrupt: false
                }),
                Event::PushContinuation(id) => s.push_continuation(mk_cont(&id)),
                Event::TurnStart => s.note_turn_started(),
                Event::TurnEnd => s.note_turn_finished(),
                Event::CancelResolve => s.note_cancel_resolved(now),
                Event::Tick(ms) => now += Duration::from_millis(ms),
            }
            let action = s.next(now);
            match action {
                ScheduledAction::ContinuationPrompt(batch) |
                ScheduledAction::MergedPrompt { batch, .. } => {
                    for c in batch.items() {
                        prop_assert!(
                            seen_scheduled_ids.insert(DelegationKey::from(c)),
                            "delegation {} attempt {} scheduled twice", c.delegation_id, c.attempt
                        );
                    }
                }
                _ => (),
            }
        }
    }

    #[test]
    fn turn_in_flight_implies_idle(events in prop::collection::vec(event_strategy(), 0..50)) {
        let mut s = BrainScheduler::new(Some(SessionId::new().into()), Arc::new(NoopSink));
        let mut now = Instant::now();
        let mut in_flight = false;

        for e in events {
            match e {
                Event::PushUser => s.push_user(InteractiveInput::Message {
                    blocks: vec![], interrupt: false
                }),
                Event::PushContinuation(id) => s.push_continuation(mk_cont(&id)),
                Event::TurnStart => { s.note_turn_started(); in_flight = true; }
                Event::TurnEnd => { s.note_turn_finished(); in_flight = false; }
                Event::CancelResolve => s.note_cancel_resolved(now),
                Event::Tick(ms) => now += Duration::from_millis(ms),
            }
            if in_flight {
                prop_assert!(matches!(s.next(now), ScheduledAction::IdleUntil { deadline: None }),
                    "scheduler returned non-idle while turn_in_flight=true");
            }
        }
    }

    #[test]
    fn pending_user_is_never_leapfrogged_by_continuation(events in prop::collection::vec(event_strategy(), 0..80)) {
        let mut s = BrainScheduler::new(Some(SessionId::new().into()), Arc::new(NoopSink));
        let mut now = Instant::now();
        let mut user_pending = false;

        for e in events {
            match e {
                Event::PushUser => { s.push_user(InteractiveInput::Message {
                    blocks: vec![], interrupt: false }); user_pending = true; }
                Event::PushContinuation(id) => s.push_continuation(mk_cont(&id)),
                Event::TurnStart => s.note_turn_started(),
                Event::TurnEnd => s.note_turn_finished(),
                Event::CancelResolve => s.note_cancel_resolved(now),
                Event::Tick(ms) => now += Duration::from_millis(ms),
            }
            let action = s.next(now);
            match action {
                ScheduledAction::ContinuationPrompt(_) => {
                    prop_assert!(!user_pending,
                        "continuation fired while user was pending");
                }
                ScheduledAction::UserPrompt(_) | ScheduledAction::MergedPrompt { .. } => {
                    user_pending = false;
                }
                _ => (),
            }
        }
    }
}
