use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use spur_acp::SpurEventBody;
use spur_core::peer_mailbox::guard::GuardOutcome;
use spur_core::peer_mailbox::ledger::{InjectionOutcome, TransitionOutcome};
use spur_core::peer_mailbox::limits::{
    aggregate_budget_for_context_window, effective_max_message_size,
};
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
async fn carry_forward_re_injects_after_pre_delivery_failure() {
    let ledger = Arc::new(InMemoryLedger::new());
    let (router, _bcast_rx) = router_with_broadcast(ledger.clone());
    let builder = PeerPromptContextBuilder::new(ledger.clone());
    let snap = snapshot();
    let target = DelegationId("tgt".into());

    let body = format!("retry budget marker {}TAIL_BEYOND_CAP", "X".repeat(450));
    let env = envelope(&body);
    let mid = env.message_id;
    let guard1 = match router.accept_or_reject(env, &snap).await.unwrap() {
        Acceptance::Created(g) => g,
        Acceptance::AlreadyAccepted => panic!("expected fresh acceptance"),
    };
    assert_eq!(ledger.get(&mid).await.unwrap().state, LedgerState::Accepted);

    let ctx1 = builder.build_for_target(&target, 32_000, 8, 2_048).await;
    assert_eq!(ctx1.injection_records.len(), 1);
    assert_eq!(ctx1.injection_records[0].message_id, mid);
    assert!(ctx1
        .orchestrator_authored_text
        .contains("retry budget marker"));

    let first = ledger
        .record_injection(&mid, &ctx1.target_prompt_id)
        .await
        .unwrap();
    assert!(matches!(first, InjectionOutcome::Injected));

    drop(guard1);
    assert_eq!(ledger.get(&mid).await.unwrap().state, LedgerState::Accepted);

    let ctx2 = builder.build_for_target(&target, 32_000, 8, 2_048).await;
    assert_eq!(ctx2.injection_records.len(), 1);
    assert_eq!(ctx2.injection_records[0].message_id, mid);
    assert_ne!(ctx2.target_prompt_id, ctx1.target_prompt_id);

    let second = ledger
        .record_injection(&mid, &ctx2.target_prompt_id)
        .await
        .unwrap();
    assert!(matches!(second, InjectionOutcome::Injected));

    let entry = ledger.get(&mid).await.unwrap();
    assert_eq!(entry.state, LedgerState::Accepted);
    assert!(entry.injected_into_prompts.contains(&ctx1.target_prompt_id));
    assert!(entry.injected_into_prompts.contains(&ctx2.target_prompt_id));
    assert_eq!(entry.injected_into_prompts.len(), 2);

    let aggregate_budget = aggregate_budget_for_context_window(32_000) as usize;
    let per_message_cap = effective_max_message_size(2_048, aggregate_budget as u64, 8);
    assert_eq!(per_message_cap, 400);
    assert!(ctx2.orchestrator_authored_text.len() <= aggregate_budget);
    assert!(!ctx2.orchestrator_authored_text.contains("TAIL_BEYOND_CAP"));
    assert!(ctx2.injection_records[0].injected_bytes as usize <= per_message_cap + 100);

    let cleanup = ledger
        .transition(&mid, LedgerState::Undeliverable)
        .await
        .unwrap();
    assert!(matches!(
        cleanup,
        TransitionOutcome::Changed {
            from: LedgerState::Accepted,
            to: LedgerState::Undeliverable
        }
    ));
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
