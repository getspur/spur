#![allow(unexpected_cfgs)]
#![cfg(madsim)]

mod event_funnel {
    use madsim_tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
    use spur_acp::domain::events::SpurEventBody;

    #[derive(Clone)]
    pub struct FunnelHandle {
        tx: UnboundedSender<SpurEventBody>,
    }

    impl FunnelHandle {
        pub fn emit(&self, body: SpurEventBody) {
            let _ = self.tx.send(body);
        }
    }

    pub fn test_channel() -> (FunnelHandle, UnboundedReceiver<SpurEventBody>) {
        let (tx, rx) = unbounded_channel();
        (FunnelHandle { tx }, rx)
    }
}

mod peer_mailbox {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::peer_message::{
        LedgerState, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
    };

    #[derive(Debug, Clone)]
    pub struct LedgerEntry {
        pub envelope: PeerMessageEnvelope,
        pub state: LedgerState,
    }

    #[derive(Default)]
    pub struct InMemoryLedger {
        entries: Mutex<HashMap<PeerMessageId, LedgerEntry>>,
    }

    impl InMemoryLedger {
        pub fn new() -> Self {
            Self::default()
        }

        pub async fn insert(&self, entry: LedgerEntry) {
            self.entries
                .lock()
                .unwrap()
                .insert(entry.envelope.message_id, entry);
        }

        pub async fn set_state(&self, message_id: &PeerMessageId, state: LedgerState) {
            self.entries
                .lock()
                .unwrap()
                .get_mut(message_id)
                .unwrap()
                .state = state;
        }

        pub async fn get(&self, message_id: &PeerMessageId) -> Option<LedgerEntry> {
            self.entries.lock().unwrap().get(message_id).cloned()
        }

        pub async fn pending_for_target(
            &self,
            target_delegation_id: &DelegationId,
        ) -> Vec<LedgerEntry> {
            self.entries
                .lock()
                .unwrap()
                .values()
                .filter(|entry| {
                    &entry.envelope.target_delegation_id == target_delegation_id
                        && matches!(entry.state, LedgerState::Accepted | LedgerState::Queued)
                })
                .cloned()
                .collect()
        }

        pub async fn non_terminal_entries(&self) -> Vec<LedgerEntry> {
            self.entries
                .lock()
                .unwrap()
                .values()
                .filter(|entry| {
                    !matches!(
                        entry.state,
                        LedgerState::Rejected
                            | LedgerState::Consumed
                            | LedgerState::Ignored
                            | LedgerState::Expired
                            | LedgerState::Dropped
                            | LedgerState::Undeliverable
                    )
                })
                .cloned()
                .collect()
        }
    }

    pub struct PeerMailboxRouter {
        ledger: Arc<InMemoryLedger>,
        funnel: crate::event_funnel::FunnelHandle,
    }

    impl PeerMailboxRouter {
        pub fn new(ledger: Arc<InMemoryLedger>, funnel: crate::event_funnel::FunnelHandle) -> Self {
            Self { ledger, funnel }
        }

        pub async fn record_terminal(
            &self,
            brain_session_id: &str,
            message_id: &PeerMessageId,
            outcome: TerminalOutcome,
        ) -> Result<(), ()> {
            let TerminalOutcome::Ignored { reason } = outcome else {
                return Err(());
            };
            self.ledger
                .set_state(message_id, LedgerState::Ignored)
                .await;
            let entry = self.ledger.get(message_id).await.ok_or(())?;
            self.funnel.emit(SpurEventBody::WorkerPeerMessageIgnored {
                brain_session_id: brain_session_id.to_string(),
                message_id: *message_id,
                target_delegation_id: entry.envelope.target_delegation_id,
                reason,
            });
            Ok(())
        }
    }

    pub struct PeerMailboxBundle {
        pub router: Arc<PeerMailboxRouter>,
        pub ledger: Arc<InMemoryLedger>,
    }
}

mod madsim_tokio_shim {
    pub mod sync {
        pub use madsim_tokio::sync::*;
    }

    pub mod time {
        use std::future::Future;
        use std::time::Duration;

        pub use madsim_tokio::time::*;

        pub async fn timeout_at<F>(deadline: Instant, future: F) -> Result<F::Output, ()>
        where
            F: Future,
        {
            let now = Instant::now();
            let timeout_duration = if deadline > now {
                deadline - now
            } else {
                Duration::ZERO
            };
            timeout(timeout_duration, future).await.map_err(|_| ())
        }
    }
}

mod orchestrator {
    pub mod delegation {
        pub mod peer_mailbox {
            use crate::madsim_tokio_shim as tokio;

            include!("../src/orchestrator/delegation/peer_mailbox.rs");
        }
    }

    use std::time::Duration;

    use madsim_tokio::sync::mpsc::UnboundedReceiver;

    pub async fn drain_peer_acks_with_timeout(
        bundle: &crate::peer_mailbox::PeerMailboxBundle,
        delegation_id: &spur_acp::domain::delegation::DelegationId,
        quiet_window: Duration,
        max_total: Duration,
        brain_session_id: &spur_acp::BrainSessionId,
        funnel: &crate::event_funnel::FunnelHandle,
        ack_rx: UnboundedReceiver<()>,
    ) {
        delegation::peer_mailbox::drain_peer_acks_with_timeout(
            bundle,
            delegation_id,
            quiet_window,
            max_total,
            brain_session_id,
            funnel,
            ack_rx,
        )
        .await;
    }
}

use std::sync::Arc;
use std::time::Duration;

use madsim_tokio::sync::mpsc::UnboundedReceiver;
use madsim_tokio::sync::mpsc::{
    unbounded_channel as sim_unbounded_channel, UnboundedReceiver as SimUnboundedReceiver,
};
use peer_mailbox::{InMemoryLedger, LedgerEntry, PeerMailboxBundle, PeerMailboxRouter};
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId,
};
use uuid::Uuid;

struct Fixture {
    bundle: PeerMailboxBundle,
    funnel: event_funnel::FunnelHandle,
    events: UnboundedReceiver<SpurEventBody>,
}

fn fixture() -> Fixture {
    let ledger = Arc::new(InMemoryLedger::new());
    let (funnel, events) = event_funnel::test_channel();
    let router = Arc::new(PeerMailboxRouter::new(ledger.clone(), funnel.clone()));

    Fixture {
        bundle: PeerMailboxBundle { router, ledger },
        funnel,
        events,
    }
}

fn fixed_peer_message_id(suffix: u128) -> PeerMessageId {
    PeerMessageId(Uuid::from_u128(suffix))
}

fn envelope(message_id: PeerMessageId, target: &DelegationId) -> PeerMessageEnvelope {
    PeerMessageEnvelope {
        schema: "spur-peer-message/v1".into(),
        message_id,
        source_delegation_id: DelegationId("src".into()),
        target_delegation_id: target.clone(),
        source_issue_id: "i1".into(),
        target_issue_id: "i2".into(),
        source_plan_task_id: "task-src".into(),
        target_plan_task_id: "task-tgt".into(),
        source_executor_id: "worker-src".into(),
        plan_version: 1,
        kind: MessageKind::Handoff,
        body: "ready".into(),
        sequence: 1,
    }
}

async fn add_delivered_message(
    fixture: &Fixture,
    target: &DelegationId,
    message_id: PeerMessageId,
) {
    fixture
        .bundle
        .ledger
        .insert(LedgerEntry {
            envelope: envelope(message_id, target),
            state: LedgerState::Delivered,
        })
        .await;
}

async fn run_drain(
    fixture: &Fixture,
    target: DelegationId,
    quiet_window: Duration,
    max_total: Duration,
    ack_rx: SimUnboundedReceiver<()>,
) -> Duration {
    let brain_session_id = spur_acp::BrainSessionId::new(spur_acp::types::SessionId("bs".into()));
    let start = madsim_tokio::time::Instant::now();
    orchestrator::drain_peer_acks_with_timeout(
        &fixture.bundle,
        &target,
        quiet_window,
        max_total,
        &brain_session_id,
        &fixture.funnel,
        ack_rx,
    )
    .await;
    start.elapsed()
}

fn drain_events(events: &mut UnboundedReceiver<SpurEventBody>) -> Vec<SpurEventBody> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

fn single_timeout_event(events: &[SpurEventBody]) -> (u32, u64) {
    let matches: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let SpurEventBody::WorkerPeerMessageDrainTimedOut {
                acks_received,
                actual_elapsed_ms,
                ..
            } = event
            {
                Some((*acks_received, *actual_elapsed_ms))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(matches.len(), 1, "expected one quiet-window timeout");
    matches[0]
}

fn single_cap_event(events: &[SpurEventBody]) -> (u32, u64) {
    let matches: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let SpurEventBody::WorkerPeerMessageDrainCappedOut {
                acks_received,
                actual_elapsed_ms,
                ..
            } = event
            {
                Some((*acks_received, *actual_elapsed_ms))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(matches.len(), 1, "expected one max-total cap event");
    matches[0]
}

fn assert_seed_from_env() {
    if let Ok(seed) = std::env::var("MADSIM_TEST_SEED") {
        assert_eq!(
            madsim::runtime::Handle::current().seed(),
            seed.parse::<u64>().expect("MADSIM_TEST_SEED must be u64"),
        );
    }
}

fn assert_elapsed_at(elapsed: Duration, expected: Duration) {
    assert!(
        elapsed >= expected && elapsed < expected + Duration::from_millis(1),
        "elapsed {elapsed:?} outside expected window starting at {expected:?}"
    );
}

#[test]
fn steady_acks_reset_quiet_window_until_producer_stops() {
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_from_env();

        let mut fixture = fixture();
        let target = DelegationId("tgt".into());
        let message_id = fixed_peer_message_id(1);
        add_delivered_message(&fixture, &target, message_id).await;

        let (ack_tx, ack_rx) = sim_unbounded_channel();
        let hold_open_until_quiet_timeout = ack_tx.clone();
        let producer = madsim_tokio::spawn(async move {
            for _ in 0..3 {
                madsim_tokio::time::sleep(Duration::from_millis(90)).await;
                ack_tx.send(()).unwrap();
            }
        });

        let elapsed = run_drain(
            &fixture,
            target,
            Duration::from_millis(100),
            Duration::from_secs(5),
            ack_rx,
        )
        .await;
        drop(hold_open_until_quiet_timeout);
        producer.await.unwrap();

        assert_elapsed_at(elapsed, Duration::from_millis(370));
        assert_eq!(
            fixture.bundle.ledger.get(&message_id).await.unwrap().state,
            LedgerState::Ignored
        );
        let events = drain_events(&mut fixture.events);
        let (acks_received, actual_elapsed_ms) = single_timeout_event(&events);
        assert_eq!(acks_received, 3);
        assert_eq!(actual_elapsed_ms, 370);
    });
}

#[test]
fn no_acks_returns_at_quiet_window_deadline() {
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_from_env();

        let mut fixture = fixture();
        let target = DelegationId("tgt".into());
        let message_id = fixed_peer_message_id(2);
        add_delivered_message(&fixture, &target, message_id).await;

        let (ack_tx, ack_rx) = sim_unbounded_channel();
        let _hold_open_until_quiet_timeout = ack_tx;

        let elapsed = run_drain(
            &fixture,
            target,
            Duration::from_millis(100),
            Duration::from_secs(5),
            ack_rx,
        )
        .await;

        assert_elapsed_at(elapsed, Duration::from_millis(100));
        assert_eq!(
            fixture.bundle.ledger.get(&message_id).await.unwrap().state,
            LedgerState::Ignored
        );
        let events = drain_events(&mut fixture.events);
        let (acks_received, actual_elapsed_ms) = single_timeout_event(&events);
        assert_eq!(acks_received, 0);
        assert_eq!(actual_elapsed_ms, 100);
    });
}

#[test]
fn steady_acks_past_max_total_stop_at_cap_and_late_acks_are_unobserved() {
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_from_env();

        let mut fixture = fixture();
        let target = DelegationId("tgt".into());
        let message_id = fixed_peer_message_id(3);
        add_delivered_message(&fixture, &target, message_id).await;

        let (ack_tx, ack_rx) = sim_unbounded_channel();
        let producer = madsim_tokio::spawn(async move {
            let mut send_results = Vec::new();
            for _ in 0..4 {
                madsim_tokio::time::sleep(Duration::from_millis(90)).await;
                send_results.push(ack_tx.send(()).is_ok());
            }
            send_results
        });

        let elapsed = run_drain(
            &fixture,
            target,
            Duration::from_millis(100),
            Duration::from_millis(250),
            ack_rx,
        )
        .await;
        let send_results = producer.await.unwrap();

        assert_elapsed_at(elapsed, Duration::from_millis(250));
        assert_eq!(send_results, vec![true, true, false, false]);
        assert_eq!(
            fixture.bundle.ledger.get(&message_id).await.unwrap().state,
            LedgerState::Ignored
        );
        let events = drain_events(&mut fixture.events);
        let (acks_received, actual_elapsed_ms) = single_cap_event(&events);
        assert_eq!(acks_received, 2);
        assert_eq!(actual_elapsed_ms, 250);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SpurEventBody::WorkerPeerMessageDrainTimedOut { .. })),
            "cap path must not also emit a quiet-window timeout"
        );
    });
}
