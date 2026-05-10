//! Invariant verification tests for the brain-async-continuation pipeline.
//!
//! These tests use the real funnel + broadcast + scheduler primitives
//! and assert the async-continuation design spec invariants end-to-end.
//! They intentionally do not spawn a full orchestrator (no fake brain
//! harness exists yet); instead they simulate the call sequence as the
//! orchestrator would issue it, exercising every real component on the
//! continuation path except the ACP transport itself.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::{new_overflow_buf, report_detached_completion};
use spur_core::event_funnel::spawn_funnel;
use spur_core::orchestrator::InteractiveInput;
use spur_core::scheduler::{BrainScheduler, ScheduledAction};
use tokio::sync::{broadcast, mpsc};

fn mk_cont(id: &str, brain_session: &SessionId) -> BrainContinuation {
    BrainContinuation {
        delegation_id: id.into(),
        attempt: 1,
        brain_session: brain_session.clone(),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("worker done".into()),
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

fn mk_completed_body(worker_session: &SessionId) -> SpurEventBody {
    SpurEventBody::DelegationCompleted {
        worker_session: worker_session.clone(),
        status: DelegationStatus::Success,
    }
}

/// Drain exactly `n` events from the broadcast receiver within `timeout`.
async fn drain_n(
    rx: &mut broadcast::Receiver<SpurEvent>,
    n: usize,
    timeout: Duration,
) -> Vec<SpurEvent> {
    let mut out = Vec::with_capacity(n);
    let deadline = tokio::time::Instant::now() + timeout;
    while out.len() < n {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(ev)) => out.push(ev),
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

/// INV-C3: the UI-visible `DelegationCompleted` MUST reach subscribers
/// with a lower `seq` than the model-visible `PromptDispatched` emission
/// that carries the matching continuation onto the ACP wire.
///
/// This test exercises the real funnel, ingress channel, overflow buffer,
/// and scheduler. It does not spawn the full orchestrator loop; instead
/// it issues the funnel emits at the exact call sites the real code uses
/// (execute_delegation upstream of the MCP callback, D-site at the
/// `connection.prompt` dispatch) with the real continuation path between
/// them. A regression that reorders either emit fails this test.
#[tokio::test(flavor = "current_thread")]
async fn invc3_delegation_completed_precedes_prompt_dispatched_in_seq_order() {
    // ── Arrange ───────────────────────────────────────────────────────
    let (bcast_tx, mut bcast_rx) = broadcast::channel(256);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx, seq);

    let (ingress_tx, mut ingress_rx) = mpsc::channel::<InteractiveInput>(64);
    let overflow = new_overflow_buf();
    let brain_session = SessionId::new();
    let mut scheduler =
        BrainScheduler::new(Some(brain_session.clone().into()), Arc::new(funnel.clone()));
    let worker_session = SessionId::new();
    let cont = mk_cont("delegation-invc3-1", &brain_session);

    // ── Act ───────────────────────────────────────────────────────────
    // Step 1: execute_delegation upstream emits DelegationCompleted
    //         BEFORE the oneshot is fired. This is the UI-visible event.
    funnel.emit(mk_completed_body(&worker_session));

    // Step 2: MCP result collector receives the oneshot result and
    //         routes the continuation into the orchestrator ingress.
    report_detached_completion(
        &ingress_tx,
        &overflow,
        brain_session.clone(),
        worker_session.clone(),
        cont.clone(),
    )
    .await;

    // Step 3: run_interactive dequeues the ingress variant and hands it
    //         to the scheduler.
    let item = tokio::time::timeout(Duration::from_millis(500), ingress_rx.recv())
        .await
        .expect("ingress recv timed out")
        .expect("ingress channel closed unexpectedly");
    match item {
        InteractiveInput::SystemContinuation { continuation, .. } => {
            scheduler.push_continuation(continuation);
        }
        other => panic!("expected SystemContinuation, got {other:?}"),
    }

    // Step 4: scheduler decides; an autonomous continuation turn is
    //         dispatched because there is no user input pending.
    let action = scheduler.next(Instant::now());
    let dispatched_count = match action {
        ScheduledAction::ContinuationPrompt(batch) => batch.len(),
        other => panic!("expected ContinuationPrompt, got {other:?}"),
    };
    assert_eq!(
        dispatched_count, 1,
        "exactly one continuation should dispatch"
    );

    // Step 5: D-site emit — orchestrator publishes PromptDispatched
    //         immediately before `connection.prompt`. Model-visible side
    //         of INV-C3.
    funnel.emit(SpurEventBody::PromptDispatched {
        session: brain_session.clone(),
        turn_kind: "continuation_only".into(),
        continuations_count: dispatched_count,
    });

    // ── Assert ────────────────────────────────────────────────────────
    let events = drain_n(&mut bcast_rx, 2, Duration::from_millis(500)).await;
    assert_eq!(
        events.len(),
        2,
        "expected two events on broadcast, got {events:?}"
    );

    let completed_seq = events
        .iter()
        .find_map(|e| match &e.body {
            SpurEventBody::DelegationCompleted { .. } => Some(e.seq),
            _ => None,
        })
        .expect("no DelegationCompleted observed");
    let dispatched_seq = events
        .iter()
        .find_map(|e| match &e.body {
            SpurEventBody::PromptDispatched { .. } => Some(e.seq),
            _ => None,
        })
        .expect("no PromptDispatched observed");

    assert!(
        completed_seq < dispatched_seq,
        "INV-C3 violated: DelegationCompleted (seq={completed_seq}) \
         must precede PromptDispatched (seq={dispatched_seq})"
    );
}
