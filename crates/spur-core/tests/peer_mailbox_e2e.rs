use serde_json::json;
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use spur_acp::SpurEventBody;
use spur_core::peer_mailbox::guard::{GuardOutcome, PeerMessageGuard};
use spur_core::peer_mailbox::ledger::{InjectionOutcome, LedgerError, TransitionOutcome};
use spur_core::peer_mailbox::limits::{
    aggregate_budget_for_context_window, effective_max_message_size,
};
use spur_core::peer_mailbox::prompt_builder::PeerPromptContextBuilder;
use spur_core::peer_mailbox::router::Acceptance;
use spur_core::peer_mailbox::{
    InMemoryLedger, Limits, PeerMailboxBundle, PeerMailboxLedger, PeerMailboxRouter, RouterError,
};
use spur_core::plan::scope_snapshot::PlanScopeSnapshot;
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
    let router = PeerMailboxRouter::new(ledger, funnel, recon_tx, Limits::default());
    (router, bcast_rx)
}

fn bundle_with_broadcast() -> (
    PeerMailboxBundle,
    broadcast::Receiver<spur_acp::SpurEvent>,
    spur_core::event_funnel::FunnelHandle,
) {
    let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
    let (bcast_tx, bcast_rx) = broadcast::channel(4096);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spur_core::event_funnel::spawn_funnel(bcast_tx.clone(), seq);
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
            brain_session_id_slot: Arc::new(tokio::sync::RwLock::new(Some("bs".into()))),
        },
        bcast_rx,
        funnel,
    )
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

fn expect_created(acceptance: Acceptance) -> PeerMessageGuard {
    if let Acceptance::AlreadyAccepted = acceptance {
        panic!("expected fresh acceptance");
    }
    if let Acceptance::Created(guard) = acceptance {
        return guard;
    }
    panic!("unexpected Acceptance variant");
}

#[tokio::test]
async fn worker_ack_during_accepted_state_consumes_message() {
    let (bundle, mut bcast_rx, funnel) = bundle_with_broadcast();
    let snap = snapshot();
    let env = envelope("Worker B: consume this during prompt");
    let mid = env.message_id;
    let _guard = expect_created(
        bundle
            .router
            .accept_or_reject("bs", env, &snap)
            .await
            .unwrap(),
    );
    assert_eq!(
        bundle.ledger.get(&mid).await.unwrap().state,
        LedgerState::Accepted
    );

    let (ack_tx, mut ack_rx) = unbounded_channel();
    spur_core::spur_ext_interp::interpret_peer_message_terminal(
        "_spur/peer_message_consumed",
        json!({ "message_id": mid }),
        &bundle,
        &ack_tx,
        &funnel,
        "bs",
        "exec-1",
    )
    .await;

    assert_eq!(
        bundle.ledger.get(&mid).await.unwrap().state,
        LedgerState::Consumed
    );
    let events = drain_broadcast_events(&mut bcast_rx).await;
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SpurEventBody::WorkerPeerMessageConsumed { message_id, .. } if *message_id == mid
        )
    }));
    assert!(ack_rx.try_recv().is_ok());
}

#[tokio::test]
async fn post_prompt_skip_via_error_arm_does_not_emit_audit_failed() {
    let (bundle, mut bcast_rx, funnel) = bundle_with_broadcast();
    let snap = Arc::new(snapshot());
    let mid = PeerMessageId(Uuid::new_v4());
    let payload = json!({
        "schema": "spur-peer-message/v1",
        "message_id": mid,
        "target_delegation_id": "tgt",
        "target_issue_id": "i2",
        "target_plan_task_id": "tb",
        "kind": "handoff",
        "body": "Worker B: consume this before post-prompt delivery audit",
        "sequence": 1
    });

    let guard = expect_created(
        spur_core::spur_ext_interp::interpret_peer_message(
            &bundle.router,
            &snap,
            DelegationId("src".into()),
            "ex".into(),
            "i1".into(),
            "ta".into(),
            "bs",
            payload,
        )
        .await
        .unwrap(),
    );

    let (ack_tx, mut ack_rx) = unbounded_channel();
    spur_core::spur_ext_interp::interpret_peer_message_terminal(
        "_spur/peer_message_consumed",
        json!({ "message_id": mid }),
        &bundle,
        &ack_tx,
        &funnel,
        "bs",
        "exec-1",
    )
    .await;
    assert!(ack_rx.try_recv().is_ok());
    assert_eq!(
        bundle.ledger.get(&mid).await.unwrap().state,
        LedgerState::Consumed
    );

    let consumed_events = drain_broadcast_events(&mut bcast_rx).await;
    assert!(consumed_events
        .iter()
        .all(|event| !matches!(event, SpurEventBody::WorkerPeerMessageAuditFailed { .. })));

    let err = bundle
        .ledger
        .transition(&mid, LedgerState::DeliveredInflight)
        .await
        .unwrap_err();
    match err {
        LedgerError::InvalidTransition { from, to } => {
            assert_eq!(from, LedgerState::Consumed);
            assert_eq!(to, LedgerState::DeliveredInflight);
        }
        other => panic!("expected terminal-source InvalidTransition, got {other:?}"),
    }

    let post_attempt_events = drain_broadcast_events(&mut bcast_rx).await;
    assert!(post_attempt_events
        .iter()
        .all(|event| !matches!(event, SpurEventBody::WorkerPeerMessageAuditFailed { .. })));

    guard
        .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
        .await;
}

#[tokio::test]
async fn full_stage1_flow_accept_inject_consume() {
    let ledger = Arc::new(InMemoryLedger::new());
    let (router, mut bcast_rx) = router_with_broadcast(ledger.clone());
    let builder = PeerPromptContextBuilder::new(ledger.clone());
    let snap = snapshot();

    let env = envelope("Worker B: please handle config validation");
    let mid = env.message_id;
    let guard = expect_created(router.accept_or_reject("bs", env, &snap).await.unwrap());
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
        .record_terminal("bs", &mid, TerminalOutcome::Consumed)
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
async fn reconcile_advance_to_delivered_then_worker_ack_consumes() {
    let (bundle, mut bcast_rx, funnel) = bundle_with_broadcast();
    let snap = snapshot();
    let env = envelope("Worker B: recover this after crash");
    let mid = env.message_id;
    let guard = expect_created(
        bundle
            .router
            .accept_or_reject("bs", env, &snap)
            .await
            .unwrap(),
    );
    assert_eq!(
        bundle.ledger.get(&mid).await.unwrap().state,
        LedgerState::Accepted
    );

    let ctx = bundle
        .builder
        .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
        .await;
    assert_eq!(ctx.injection_records.len(), 1);
    assert_eq!(ctx.injection_records[0].message_id, mid);
    bundle
        .ledger
        .record_injection(&mid, &ctx.target_prompt_id)
        .await
        .unwrap();
    bundle
        .ledger
        .transition(&mid, LedgerState::DeliveredInflight)
        .await
        .unwrap();

    spur_core::peer_mailbox::reconciler::run_startup_reconcile(
        bundle.ledger.clone(),
        funnel.clone(),
        "bs".into(),
        Duration::from_millis(100),
    )
    .await;

    assert_eq!(
        bundle.ledger.get(&mid).await.unwrap().state,
        LedgerState::Delivered
    );
    let delivered_events = drain_broadcast_events(&mut bcast_rx).await;
    assert!(delivered_events.iter().any(|event| {
        matches!(
            event,
            SpurEventBody::WorkerPeerMessageDelivered { message_id, .. } if *message_id == mid
        )
    }));

    let (ack_tx, mut ack_rx) = unbounded_channel();
    spur_core::spur_ext_interp::interpret_peer_message_terminal(
        "_spur/peer_message_consumed",
        json!({ "message_id": mid }),
        &bundle,
        &ack_tx,
        &funnel,
        "bs",
        "exec-1",
    )
    .await;

    assert_eq!(
        bundle.ledger.get(&mid).await.unwrap().state,
        LedgerState::Consumed
    );
    let consumed_events = drain_broadcast_events(&mut bcast_rx).await;
    assert!(consumed_events.iter().any(|event| {
        matches!(
            event,
            SpurEventBody::WorkerPeerMessageConsumed { message_id, .. } if *message_id == mid
        )
    }));
    assert!(ack_rx.try_recv().is_ok());

    guard
        .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
        .await;
}

#[tokio::test]
async fn malformed_terminal_notification_emits_malformed_funnel_event() {
    let (bundle, mut bcast_rx, funnel) = bundle_with_broadcast();
    let (ack_tx, mut ack_rx) = unbounded_channel();

    spur_core::spur_ext_interp::interpret_peer_message_terminal(
        "_spur/peer_message_consumed",
        json!({ "message_id": null }),
        &bundle,
        &ack_tx,
        &funnel,
        "bs",
        "exec-1",
    )
    .await;

    assert!(ack_rx.try_recv().is_ok());
    let events = drain_broadcast_events(&mut bcast_rx).await;
    let malformed_events = events
        .iter()
        .filter(|event| matches!(event, SpurEventBody::WorkerPeerMessageMalformed { .. }))
        .count();
    assert_eq!(malformed_events, 1);
    assert!(events
        .iter()
        .all(|event| !matches!(event, SpurEventBody::WorkerPeerMessageConsumed { .. })));
    assert!(events
        .iter()
        .all(|event| !matches!(event, SpurEventBody::WorkerPeerMessageIgnored { .. })));
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
    let guard1 = expect_created(router.accept_or_reject("bs", env, &snap).await.unwrap());
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
        .accept_or_reject("bs", envelope("blocked"), &snap)
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
