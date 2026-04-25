use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use spur_acp::SpurEventBody;
use spur_core::peer_mailbox::guard::GuardOutcome;
use spur_core::peer_mailbox::ledger::{InjectionOutcome, TransitionOutcome};
use spur_core::peer_mailbox::prompt_builder::PeerPromptContextBuilder;
use spur_core::peer_mailbox::router::Acceptance;
use spur_core::peer_mailbox::{
    InMemoryLedger, Limits, PeerMailboxLedger, PeerMailboxRouter, RouterError,
};
use spur_mcp::plan::scope_snapshot::PlanScopeSnapshot;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc::unbounded_channel};
use uuid::Uuid;

fn snapshot() -> PlanScopeSnapshot {
    let mut delegation_to_task = HashMap::new();
    delegation_to_task.insert(DelegationId("src".into()), "ta".into());
    delegation_to_task.insert(DelegationId("tgt".into()), "tb".into());

    let mut peer_edges = HashSet::new();
    peer_edges.insert(("ta".into(), "tb".into()));

    PlanScopeSnapshot {
        plan_version: 1,
        peer_edges,
        delegation_to_task,
        delegation_to_issue: HashMap::new(),
        superseded_tasks: HashSet::new(),
        terminal_tasks: HashSet::new(),
    }
}

fn envelope(body: &str) -> PeerMessageEnvelope {
    PeerMessageEnvelope {
        schema: "spur-peer-message/v1".into(),
        message_id: PeerMessageId(Uuid::new_v4()),
        source_delegation_id: DelegationId("src".into()),
        target_delegation_id: DelegationId("tgt".into()),
        source_issue_id: "i1".into(),
        target_issue_id: "i2".into(),
        source_plan_task_id: "ta".into(),
        target_plan_task_id: "tb".into(),
        source_executor_id: "ex".into(),
        plan_version: 1,
        kind: MessageKind::Handoff,
        body: body.into(),
        sequence: 1,
    }
}

fn router_with_broadcast(
    ledger: Arc<InMemoryLedger>,
) -> (PeerMailboxRouter, broadcast::Receiver<spur_acp::SpurEvent>) {
    let (bcast_tx, bcast_rx) = broadcast::channel(4096);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spur_core::event_funnel::spawn_funnel(bcast_tx.clone(), seq);
    let (recon_tx, _recon_rx) = unbounded_channel();
    let router = PeerMailboxRouter::new(ledger, funnel, recon_tx, Limits::default(), "bs".into());
    (router, bcast_rx)
}

async fn drain_broadcast_events(
    bcast_rx: &mut broadcast::Receiver<spur_acp::SpurEvent>,
) -> Vec<SpurEventBody> {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(20), bcast_rx.recv()).await {
            Ok(Ok(event)) => out.push(event.body),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
        }
    }
    out
}

#[tokio::test]
async fn full_stage1_flow_accept_inject_consume() {
    let ledger = Arc::new(InMemoryLedger::new());
    let (router, mut bcast_rx) = router_with_broadcast(ledger.clone());
    let builder = PeerPromptContextBuilder::new(ledger.clone());
    let snap = snapshot();

    let env = envelope("Worker B: please handle config validation");
    let mid = env.message_id;
    let guard = match router.accept_or_reject(env, &snap).await.unwrap() {
        Acceptance::Created(g) => g,
        Acceptance::AlreadyAccepted => panic!("expected fresh acceptance"),
    };
    assert_eq!(ledger.get(&mid).await.unwrap().state, LedgerState::Accepted);

    let ctx = builder
        .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
        .await;
    assert_eq!(ctx.injection_records.len(), 1);
    assert_eq!(ctx.injection_records[0].message_id, mid);
    assert!(ctx.orchestrator_authored_text.contains("config validation"));

    let first = ledger
        .record_injection(&mid, &ctx.target_prompt_id)
        .await
        .unwrap();
    let second = ledger
        .record_injection(&mid, &ctx.target_prompt_id)
        .await
        .unwrap();
    assert!(matches!(first, InjectionOutcome::Injected));
    assert!(matches!(second, InjectionOutcome::AlreadyInjected));

    let inflight = ledger
        .transition(&mid, LedgerState::DeliveredInflight)
        .await
        .unwrap();
    let delivered = ledger
        .transition(&mid, LedgerState::Delivered)
        .await
        .unwrap();
    assert!(matches!(
        inflight,
        TransitionOutcome::Changed {
            from: LedgerState::Accepted,
            to: LedgerState::DeliveredInflight
        }
    ));
    assert!(matches!(
        delivered,
        TransitionOutcome::Changed {
            from: LedgerState::DeliveredInflight,
            to: LedgerState::Delivered
        }
    ));

    router
        .record_terminal(&mid, TerminalOutcome::Consumed)
        .await
        .unwrap();
    assert_eq!(ledger.get(&mid).await.unwrap().state, LedgerState::Consumed);

    guard
        .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
        .await;

    let events = drain_broadcast_events(&mut bcast_rx).await;
    assert!(events
        .iter()
        .any(|event| matches!(event, SpurEventBody::WorkerPeerMessageAccepted { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, SpurEventBody::WorkerPeerMessageConsumed { .. })));
}

#[tokio::test]
async fn rejected_message_is_not_in_pending() {
    let ledger = Arc::new(InMemoryLedger::new());
    let (router, _bcast_rx) = router_with_broadcast(ledger.clone());
    let mut snap = snapshot();
    snap.peer_edges.clear();

    let err = router
        .accept_or_reject(envelope("blocked"), &snap)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        RouterError::Rejected {
            reason: "not_in_dag".into()
        }
    );

    let pending = ledger.pending_for_target(&DelegationId("tgt".into())).await;
    assert!(pending.is_empty());
}
