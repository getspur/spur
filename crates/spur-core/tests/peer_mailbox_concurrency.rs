//! Peer mailbox concurrency invariants.
//!
//! `PEER_MAILBOX_CONCURRENCY_N` optionally overrides the distinct-envelope
//! contention count used by `n_task_accept_race_with_distinct_envelopes`.

use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use spur_core::event_funnel::FunnelHandle;
use spur_core::peer_mailbox::guard::PeerMessageGuard;
use spur_core::peer_mailbox::ledger::{InjectionOutcome, LedgerError, TransitionOutcome};
use spur_core::peer_mailbox::prompt_builder::PeerPromptContextBuilder;
use spur_core::peer_mailbox::router::Acceptance;
use spur_core::peer_mailbox::{
    InMemoryLedger, Limits, PeerMailboxBundle, PeerMailboxLedger, PeerMailboxRouter,
};
use spur_core::plan::scope_snapshot::PlanScopeSnapshot;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use uuid::Uuid;

const PLAN_VERSION: u64 = 1;

fn id(n: u128) -> PeerMessageId {
    PeerMessageId(Uuid::from_u128(n))
}

fn delegation(name: &str) -> DelegationId {
    DelegationId(name.into())
}

fn task_for_delegation(name: &str) -> String {
    format!("task-{name}")
}

fn snapshot_for_targets(targets: &[&str]) -> PlanScopeSnapshot {
    let mut delegation_to_task = HashMap::new();
    delegation_to_task.insert(delegation("src"), task_for_delegation("src"));

    let mut peer_edges = HashSet::new();
    for target in targets {
        let target_task = task_for_delegation(target);
        delegation_to_task.insert(delegation(target), target_task.clone());
        peer_edges.insert((task_for_delegation("src"), target_task));
    }

    PlanScopeSnapshot {
        plan_version: PLAN_VERSION,
        peer_edges,
        delegation_to_task,
        delegation_to_issue: HashMap::new(),
        superseded_tasks: HashSet::new(),
        terminal_tasks: HashSet::new(),
    }
}

fn envelope(message_id: PeerMessageId, target: &str, sequence: u64) -> PeerMessageEnvelope {
    PeerMessageEnvelope {
        schema: "spur-peer-message/v1".into(),
        message_id,
        source_delegation_id: delegation("src"),
        target_delegation_id: delegation(target),
        source_issue_id: "issue-src".into(),
        target_issue_id: format!("issue-{target}"),
        source_plan_task_id: task_for_delegation("src"),
        target_plan_task_id: task_for_delegation(target),
        source_executor_id: "executor-src".into(),
        plan_version: PLAN_VERSION,
        kind: MessageKind::Handoff,
        body: format!("message {sequence} for {target}"),
        sequence,
    }
}

fn fixture() -> (
    Arc<InMemoryLedger>,
    Arc<PeerMailboxRouter>,
    FunnelHandle,
    UnboundedReceiver<SpurEventBody>,
) {
    let ledger = Arc::new(InMemoryLedger::new());
    let (funnel, events) = spur_core::event_funnel::test_channel();
    let (recon_tx, _recon_rx) = unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel.clone(),
        recon_tx,
        Limits::default(),
    ));
    (ledger, router, funnel, events)
}

fn bundle_fixture() -> (
    PeerMailboxBundle,
    FunnelHandle,
    UnboundedReceiver<SpurEventBody>,
) {
    let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
    let (funnel, events) = spur_core::event_funnel::test_channel();
    let (recon_tx, _recon_rx) = unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel.clone(),
        recon_tx,
        Limits::default(),
    ));
    let builder = Arc::new(PeerPromptContextBuilder::new(ledger.clone()));

    (
        PeerMailboxBundle {
            router,
            builder,
            ledger,
            brain_session_id_slot: Arc::new(tokio::sync::RwLock::new(Some("brain-session".into()))),
        },
        funnel,
        events,
    )
}

async fn flush_funnel(funnel: &FunnelHandle) {
    let _ = funnel.lineage_snapshot().await;
}

fn drain_events(events: &mut UnboundedReceiver<SpurEventBody>) -> Vec<SpurEventBody> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

fn expect_created(acceptance: Acceptance) -> PeerMessageGuard {
    if let Acceptance::AlreadyAccepted = acceptance {
        panic!("expected fresh acceptance");
    }
    if let Acceptance::Created(guard) = acceptance {
        return guard;
    }
    panic!("unexpected Acceptance variant");
}

fn distinct_accept_stress_n() -> usize {
    std::env::var("PEER_MAILBOX_CONCURRENCY_N")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(256)
}

async fn run_distinct_accept_race(n: usize, id_base: u128) {
    let (ledger, router, funnel, mut events) = fixture();
    let snapshot = Arc::new(snapshot_for_targets(&["tgt"]));
    let target = delegation("tgt");
    let barrier = Arc::new(Barrier::new(n + 1));
    let mut set = JoinSet::new();

    for index in 0..n {
        let router = router.clone();
        let snapshot = snapshot.clone();
        let barrier = barrier.clone();
        let env = envelope(id(id_base + index as u128), "tgt", index as u64);
        set.spawn(async move {
            barrier.wait().await;
            router
                .accept_or_reject("brain-session", env, &snapshot)
                .await
        });
    }

    barrier.wait().await;

    let results = set.join_all().await;
    let created = results
        .into_iter()
        .map(|result| result.expect("accept should succeed"))
        .filter(|acceptance| matches!(acceptance, Acceptance::Created(_)))
        .count();
    assert_eq!(created, n);

    flush_funnel(&funnel).await;
    let _ = drain_events(&mut events);

    let entries = ledger.pending_for_target(&target).await;
    assert_eq!(entries.len(), n);

    let actual: HashSet<_> = entries
        .iter()
        .map(|entry| entry.envelope.message_id)
        .collect();
    let expected: HashSet<_> = (0..n).map(|index| id(id_base + index as u128)).collect();
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn n_task_accept_race_with_same_envelope() {
    const N: usize = 32;

    let (ledger, router, funnel, mut events) = fixture();
    let snapshot = Arc::new(snapshot_for_targets(&["tgt"]));
    let message_id = id(0x100);
    let env = envelope(message_id, "tgt", 1);
    let barrier = Arc::new(Barrier::new(N + 1));
    let mut set = JoinSet::new();

    for _ in 0..N {
        let router = router.clone();
        let snapshot = snapshot.clone();
        let env = env.clone();
        let barrier = barrier.clone();
        set.spawn(async move {
            barrier.wait().await;
            router
                .accept_or_reject("brain-session", env, &snapshot)
                .await
        });
    }

    barrier.wait().await;

    let mut created = Vec::new();
    let mut already_accepted = 0;
    for result in set.join_all().await {
        let acceptance = result.expect("accept should succeed");
        if let Acceptance::AlreadyAccepted = acceptance {
            already_accepted += 1;
        } else {
            created.push(expect_created(acceptance));
        }
    }

    assert_eq!(created.len(), 1);
    assert_eq!(already_accepted, N - 1);

    drop(created);
    assert_eq!(
        ledger.get(&message_id).await.expect("ledger entry").state,
        LedgerState::Accepted
    );

    flush_funnel(&funnel).await;
    let accepted_events = drain_events(&mut events)
        .into_iter()
        .filter(|event| matches!(event, SpurEventBody::WorkerPeerMessageAccepted { .. }))
        .count();
    assert_eq!(accepted_events, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn n_task_accept_race_with_distinct_envelopes() {
    run_distinct_accept_race(64, 0x200).await;
    run_distinct_accept_race(distinct_accept_stress_n(), 0x400).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_transitions_on_same_message_are_serialized() {
    const N: usize = 16;

    let ledger = Arc::new(InMemoryLedger::new());
    let message_id = id(0x800);
    ledger
        .accept(envelope(message_id, "tgt", 1))
        .await
        .expect("accept should succeed");

    let targets = [
        LedgerState::Accepted,
        LedgerState::Queued,
        LedgerState::DeliveredInflight,
        LedgerState::Delivered,
        LedgerState::Consumed,
        LedgerState::Ignored,
        LedgerState::Expired,
        LedgerState::Dropped,
        LedgerState::Undeliverable,
        LedgerState::Rejected,
        LedgerState::Unknown,
        LedgerState::Queued,
        LedgerState::DeliveredInflight,
        LedgerState::Consumed,
        LedgerState::Ignored,
        LedgerState::Dropped,
    ];
    let barrier = Arc::new(Barrier::new(N + 1));
    let err_count = Arc::new(AtomicUsize::new(0));
    let mut set = JoinSet::new();

    for target in targets {
        let ledger = ledger.clone();
        let barrier = barrier.clone();
        let err_count = err_count.clone();
        set.spawn(async move {
            barrier.wait().await;
            let result = ledger.transition(&message_id, target).await;
            if matches!(result, Err(LedgerError::InvalidTransition { .. })) {
                err_count.fetch_add(1, Ordering::SeqCst);
            }
            (target, result)
        });
    }

    barrier.wait().await;

    let results = set.join_all().await;
    let ok_terminal_states: Vec<_> = results
        .iter()
        .filter_map(|(target, result)| match result {
            Ok(TransitionOutcome::Changed { to, .. }) => Some(*to),
            Ok(TransitionOutcome::Unchanged(state)) => Some(*state),
            Err(_) => {
                let _ = target;
                None
            }
        })
        .collect();

    assert!(
        !ok_terminal_states.is_empty(),
        "at least one serialized transition should succeed"
    );

    let final_state = ledger.get(&message_id).await.expect("ledger entry").state;
    assert!(
        ok_terminal_states.contains(&final_state),
        "final state {final_state:?} was not produced by an accepted transition: {results:?}"
    );
    assert!(
        err_count.load(Ordering::SeqCst) >= 1,
        "expected at least one invalid transition rejection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fan_out_100x100_pending_for_target_is_consistent() {
    let ledger = Arc::new(InMemoryLedger::new());
    let target_a = delegation("tgt-A");
    let target_b = delegation("tgt-B");
    let expected_a: HashSet<_> = (0..100).map(|index| id(0x1_000 + index)).collect();
    let expected_b: HashSet<_> = (0..100).map(|index| id(0x2_000 + index)).collect();

    for (index, message_id) in expected_a.iter().copied().enumerate() {
        ledger
            .accept(envelope(message_id, "tgt-A", index as u64))
            .await
            .expect("accept A should succeed");
    }
    for (index, message_id) in expected_b.iter().copied().enumerate() {
        ledger
            .accept(envelope(message_id, "tgt-B", index as u64))
            .await
            .expect("accept B should succeed");
    }

    let barrier = Arc::new(Barrier::new(201));
    let mut set = JoinSet::new();
    for index in 0..200 {
        let ledger = ledger.clone();
        let target = if index % 2 == 0 {
            target_a.clone()
        } else {
            target_b.clone()
        };
        let barrier = barrier.clone();
        set.spawn(async move {
            barrier.wait().await;
            let actual: HashSet<_> = ledger
                .pending_for_target(&target)
                .await
                .iter()
                .map(|entry| entry.envelope.message_id)
                .collect();
            (target, actual)
        });
    }

    barrier.wait().await;

    let mut target_a_sets = Vec::new();
    let mut target_b_sets = Vec::new();
    for (target, actual) in set.join_all().await {
        if target == target_a {
            assert_eq!(actual, expected_a);
            target_a_sets.push(actual);
        } else {
            assert_eq!(target, target_b);
            assert_eq!(actual, expected_b);
            target_b_sets.push(actual);
        }
    }

    assert_eq!(target_a_sets.len(), 100);
    assert_eq!(target_b_sets.len(), 100);
    let h0 = target_a_sets[0].clone();
    for h in &target_a_sets[1..] {
        assert_eq!(h, &h0);
    }
    let h0 = target_b_sets[0].clone();
    for h in &target_b_sets[1..] {
        assert_eq!(h, &h0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn record_injection_concurrent_calls_are_idempotent() {
    const N: usize = 32;

    let ledger = Arc::new(InMemoryLedger::new());
    let message_id = id(0x3_000);
    ledger
        .accept(envelope(message_id, "tgt", 1))
        .await
        .expect("accept should succeed");

    let barrier = Arc::new(Barrier::new(N + 1));
    let mut set = JoinSet::new();
    for _ in 0..N {
        let ledger = ledger.clone();
        let barrier = barrier.clone();
        set.spawn(async move {
            barrier.wait().await;
            ledger.record_injection(&message_id, "prompt-X").await
        });
    }

    barrier.wait().await;

    let mut injected = 0;
    let mut already_injected = 0;
    for result in set.join_all().await {
        match result.expect("record_injection should succeed") {
            InjectionOutcome::Injected => injected += 1,
            InjectionOutcome::AlreadyInjected => already_injected += 1,
        }
    }

    assert_eq!(injected, 1);
    assert_eq!(already_injected, N - 1);

    let entry = ledger.get(&message_id).await.expect("ledger entry");
    assert_eq!(entry.injected_into_prompts.len(), 1);
    assert!(entry.injected_into_prompts.contains("prompt-X"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_replay_after_acceptance_returns_already_accepted() {
    const N: usize = 8;

    let ledger = Arc::new(InMemoryLedger::new());
    let (funnel, mut events) = spur_core::event_funnel::test_channel();
    let (recon_tx, _recon_rx) = unbounded_channel();
    let router =
        PeerMailboxRouter::new(ledger.clone(), funnel.clone(), recon_tx, Limits::default());
    let snapshot = snapshot_for_targets(&["tgt"]);
    let env = envelope(id(0x3_100), "tgt", 1);

    let guard = expect_created(
        router
            .accept_or_reject("brain-session", env.clone(), &snapshot)
            .await
            .expect("first accept should succeed"),
    );
    drop(guard);

    for _ in 0..N {
        let acceptance = router
            .accept_or_reject("brain-session", env.clone(), &snapshot)
            .await
            .expect("replay accept should succeed");
        if let Acceptance::Created(_) = &acceptance {
            panic!("replay returned a second guard");
        }
        if let Acceptance::AlreadyAccepted = &acceptance {
        } else {
            panic!("replay returned an unexpected Acceptance variant");
        }
    }

    flush_funnel(&funnel).await;
    let accepted_events = drain_events(&mut events)
        .into_iter()
        .filter(|event| matches!(event, SpurEventBody::WorkerPeerMessageAccepted { .. }))
        .count();
    assert_eq!(accepted_events, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_record_terminal_does_not_double_emit() {
    const N: usize = 16;

    let (ledger, router, funnel, mut events) = fixture();
    let snapshot = snapshot_for_targets(&["tgt"]);
    let message_id = id(0x3_200);
    let env = envelope(message_id, "tgt", 1);
    let guard = expect_created(
        router
            .accept_or_reject("brain-session", env, &snapshot)
            .await
            .expect("accept should succeed"),
    );
    ledger
        .transition(&message_id, LedgerState::Queued)
        .await
        .expect("queue transition should succeed");
    ledger
        .transition(&message_id, LedgerState::DeliveredInflight)
        .await
        .expect("inflight transition should succeed");
    ledger
        .transition(&message_id, LedgerState::Delivered)
        .await
        .expect("delivered transition should succeed");

    let barrier = Arc::new(Barrier::new(N + 1));
    let mut set = JoinSet::new();
    for _ in 0..N {
        let router = router.clone();
        let barrier = barrier.clone();
        set.spawn(async move {
            barrier.wait().await;
            router
                .record_terminal("brain-session", &message_id, TerminalOutcome::Consumed)
                .await
        });
    }

    barrier.wait().await;

    for result in set.join_all().await {
        result.expect("record_terminal should succeed");
    }
    drop(guard);

    flush_funnel(&funnel).await;
    let consumed_events = drain_events(&mut events)
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                SpurEventBody::WorkerPeerMessageConsumed { message_id: id, .. } if *id == message_id
            )
        })
        .count();
    assert_eq!(consumed_events, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconciler_does_not_race_with_concurrent_workers() {
    const N: usize = 24;

    let (bundle, funnel, mut events) = bundle_fixture();
    let router = bundle.router.clone();
    let ledger = bundle.ledger.clone();
    let snapshot = Arc::new(snapshot_for_targets(&["tgt"]));
    let ready = Arc::new(Barrier::new(N + 1));
    let mut set = JoinSet::new();
    let mut ids = Vec::new();

    for index in 0..N {
        let router = router.clone();
        let ledger = ledger.clone();
        let snapshot = snapshot.clone();
        let ready = ready.clone();
        let message_id = id(0x4_000 + index as u128);
        ids.push((message_id, index % 2 == 0));
        set.spawn(async move {
            let env = envelope(message_id, "tgt", index as u64);
            let guard = expect_created(
                router
                    .accept_or_reject("brain-session", env, &snapshot)
                    .await
                    .unwrap(),
            );
            ready.wait().await;
            ledger
                .transition(&message_id, LedgerState::Queued)
                .await
                .expect("queue transition should succeed");
            if index % 2 == 0 {
                ledger
                    .record_injection(&message_id, "prompt-X")
                    .await
                    .expect("record injection should succeed");
            }
            ledger
                .transition(&message_id, LedgerState::DeliveredInflight)
                .await
                .expect("inflight transition should succeed");
            let _ = router
                .record_terminal("brain-session", &message_id, TerminalOutcome::Consumed)
                .await;
            drop(guard);
        });
    }

    ready.wait().await;
    let counts = spur_core::peer_mailbox::reconciler::run_startup_reconcile(
        ledger.clone(),
        funnel.clone(),
        "brain-session".into(),
        Duration::from_millis(1),
    )
    .await;

    set.join_all().await;

    assert!(
        counts.inflight_forced_to_delivered + counts.inflight_stranded <= N as u32,
        "reconcile counts should not exceed raced messages: {counts:?}"
    );

    for (message_id, injected) in ids {
        let state = ledger.get(&message_id).await.expect("ledger entry").state;
        match state {
            LedgerState::Delivered => assert!(
                injected,
                "message {message_id:?} was delivered without injection"
            ),
            LedgerState::Queued => assert!(
                !injected,
                "message {message_id:?} was reverted despite recorded injection"
            ),
            LedgerState::DeliveredInflight => assert!(
                !injected,
                "message {message_id:?} was stranded despite recorded injection"
            ),
            LedgerState::Ignored | LedgerState::Consumed => {}
            other => panic!(
                "message {message_id:?} ended in unexpected state {other:?}; injected={injected}"
            ),
        }
        assert_ne!(
            state,
            LedgerState::DeliveredInflight,
            "message {message_id:?} remained inflight after reconcile"
        );
    }

    flush_funnel(&funnel).await;
    let reconciled_events = drain_events(&mut events)
        .into_iter()
        .filter(|event| matches!(event, SpurEventBody::WorkerPeerMailboxReconciled { .. }))
        .count();
    assert_eq!(reconciled_events, 1);
}
