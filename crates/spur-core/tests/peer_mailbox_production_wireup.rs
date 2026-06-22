use serde_json::json;
use spur_acp::config::SpurConfig;
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::{LedgerState, MessageKind, PeerMessageId};
use spur_core::orchestrator::Orchestrator;
use spur_core::peer_mailbox::guard::GuardOutcome;
use spur_core::peer_mailbox::router::Acceptance;
use spur_core::peer_mailbox::RouterError;
use spur_core::plan::scope_snapshot::PlanScopeSnapshot;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use uuid::Uuid;

fn enabled_config() -> SpurConfig {
    SpurConfig {
        peer_mailbox_enabled: true,
        ..SpurConfig::default()
    }
}

fn snapshot() -> Arc<PlanScopeSnapshot> {
    let mut delegation_to_task = HashMap::new();
    delegation_to_task.insert(DelegationId("src".into()), "task-src".into());
    delegation_to_task.insert(DelegationId("tgt".into()), "task-tgt".into());

    let mut peer_edges = HashSet::new();
    peer_edges.insert(("task-src".into(), "task-tgt".into()));

    Arc::new(PlanScopeSnapshot {
        plan_version: 1,
        peer_edges,
        delegation_to_task,
        delegation_to_issue: HashMap::new(),
        superseded_tasks: HashSet::new(),
        terminal_tasks: HashSet::new(),
    })
}

fn payload(message_id: PeerMessageId) -> serde_json::Value {
    json!({
        "schema": "spur-peer-message/v1",
        "message_id": message_id,
        "target_delegation_id": "tgt",
        "target_issue_id": "issue-tgt",
        "target_plan_task_id": "task-tgt",
        "kind": MessageKind::Handoff,
        "body": "handoff from src to tgt",
        "sequence": 1
    })
}

async fn drain_events(rx: &mut broadcast::Receiver<spur_acp::SpurEvent>) -> Vec<SpurEventBody> {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(20), rx.recv()).await {
            Ok(Ok(event)) => out.push(event.body),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
        }
    }
    out
}

async fn wait_for_state(
    orchestrator: &Orchestrator,
    message_id: &PeerMessageId,
    expected: LedgerState,
) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let state = orchestrator
            .peer_mailbox_bundle()
            .expect("peer mailbox bundle")
            .ledger
            .get(message_id)
            .await
            .map(|entry| entry.state);
        if state == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {message_id:?} to reach {expected:?}; last state {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn emit_peer_message(
    orchestrator: &Orchestrator,
    brain_session_id: &str,
    message_id: PeerMessageId,
) -> Option<Result<Acceptance, RouterError>> {
    let bundle = orchestrator.peer_mailbox_bundle()?;
    Some(
        spur_core::spur_ext_interp::interpret_peer_message(
            &bundle.router,
            &snapshot(),
            DelegationId("src".into()),
            "executor-src".into(),
            "issue-src".into(),
            "task-src".into(),
            brain_session_id,
            payload(message_id),
        )
        .await,
    )
}

fn expect_created(acceptance: Acceptance) -> spur_core::peer_mailbox::PeerMessageGuard {
    if let Acceptance::AlreadyAccepted = acceptance {
        panic!("expected fresh acceptance");
    }
    if let Acceptance::Created(guard) = acceptance {
        return guard;
    }
    panic!("unexpected acceptance variant");
}

#[tokio::test]
async fn peer_mailbox_enabled_true_attaches_bundle_and_spawns_reconciler() {
    let tmp = tempfile::tempdir().unwrap();
    let orchestrator = Orchestrator::new(tmp.path().into(), enabled_config(), None).unwrap();
    let mut events = orchestrator.subscribe();
    assert!(orchestrator.peer_mailbox_bundle().is_some());
    assert!(orchestrator
        .peer_mailbox_reconciler_abort_handle()
        .is_some());

    let message_id = PeerMessageId(Uuid::new_v4());
    let guard = expect_created(
        emit_peer_message(&orchestrator, "session-true", message_id)
            .await
            .expect("peer mailbox enabled")
            .unwrap(),
    );
    guard
        .finalize(GuardOutcome::Terminal(
            spur_acp::domain::peer_message::TerminalOutcome::Consumed,
        ))
        .await;

    let entry = orchestrator
        .peer_mailbox_bundle()
        .unwrap()
        .ledger
        .get(&message_id)
        .await
        .expect("accepted ledger entry");
    assert_eq!(entry.state, LedgerState::Accepted);

    let events = drain_events(&mut events).await;
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SpurEventBody::WorkerPeerMessageAccepted {
                brain_session_id,
                message_id: event_message_id,
                ..
            } if brain_session_id == "session-true" && *event_message_id == message_id
        )
    }));
}

#[tokio::test]
async fn peer_mailbox_enabled_false_silently_drops_notification() {
    let tmp = tempfile::tempdir().unwrap();
    let orchestrator = Orchestrator::new(tmp.path().into(), SpurConfig::default(), None).unwrap();
    let mut events = orchestrator.subscribe();
    assert!(orchestrator.peer_mailbox_bundle().is_none());

    let message_id = PeerMessageId(Uuid::new_v4());
    assert!(
        emit_peer_message(&orchestrator, "session-default", message_id)
            .await
            .is_none()
    );

    let events = drain_events(&mut events).await;
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            SpurEventBody::WorkerPeerMessageAccepted { .. }
                | SpurEventBody::WorkerPeerMessageRejected { .. }
                | SpurEventBody::WorkerPeerMessageUndeliverable { .. }
        )
    }));
}

#[tokio::test]
async fn reconciler_drains_stranded_message() {
    let tmp = tempfile::tempdir().unwrap();
    let orchestrator = Orchestrator::new(tmp.path().into(), enabled_config(), None).unwrap();
    let mut events = orchestrator.subscribe();
    let bundle = orchestrator.peer_mailbox_bundle().expect("peer mailbox");
    *bundle.brain_session_id_slot.write().await = Some("session-reconcile".into());

    let message_id = PeerMessageId(Uuid::new_v4());
    drop(expect_created(
        emit_peer_message(&orchestrator, "session-reconcile", message_id)
            .await
            .expect("peer mailbox enabled")
            .unwrap(),
    ));

    wait_for_state(&orchestrator, &message_id, LedgerState::Undeliverable).await;
    let events = drain_events(&mut events).await;
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SpurEventBody::WorkerPeerMessageUndeliverable {
                brain_session_id,
                message_id: event_message_id,
                ..
            } if brain_session_id == "session-reconcile" && *event_message_id == message_id
        )
    }));
}

#[tokio::test]
async fn orchestrator_drop_aborts_reconciler() {
    let tmp = tempfile::tempdir().unwrap();
    let orchestrator = Orchestrator::new(tmp.path().into(), enabled_config(), None).unwrap();
    let handle = orchestrator
        .peer_mailbox_reconciler_abort_handle()
        .expect("reconciler handle");

    drop(orchestrator);

    let deadline = Instant::now() + Duration::from_secs(1);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "reconciler was not aborted");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn session_slot_update_propagates_to_reconciler_emit() {
    let tmp = tempfile::tempdir().unwrap();
    let orchestrator = Orchestrator::new(tmp.path().into(), enabled_config(), None).unwrap();
    let mut events = orchestrator.subscribe();
    let bundle = orchestrator.peer_mailbox_bundle().expect("peer mailbox");

    *bundle.brain_session_id_slot.write().await = Some("session-A".into());
    let first_id = PeerMessageId(Uuid::new_v4());
    drop(expect_created(
        emit_peer_message(&orchestrator, "session-A", first_id)
            .await
            .expect("peer mailbox enabled")
            .unwrap(),
    ));
    wait_for_state(&orchestrator, &first_id, LedgerState::Undeliverable).await;

    *bundle.brain_session_id_slot.write().await = Some("session-B".into());
    let second_id = PeerMessageId(Uuid::new_v4());
    drop(expect_created(
        emit_peer_message(&orchestrator, "session-B", second_id)
            .await
            .expect("peer mailbox enabled")
            .unwrap(),
    ));
    wait_for_state(&orchestrator, &second_id, LedgerState::Undeliverable).await;

    let events = drain_events(&mut events).await;
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SpurEventBody::WorkerPeerMessageUndeliverable {
                brain_session_id,
                message_id,
                ..
            } if brain_session_id == "session-A" && *message_id == first_id
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SpurEventBody::WorkerPeerMessageUndeliverable {
                brain_session_id,
                message_id,
                ..
            } if brain_session_id == "session-B" && *message_id == second_id
        )
    }));
}
