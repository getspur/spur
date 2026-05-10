//! Regression test for the StaleSession continuation-drop bug introduced
//! in commit 5c50e24 (feat(spur-core,spur-mcp): S3.1 implement continuation
//! bridge v2, 2026-04-24).
//!
//! ## The bug
//!
//! S3.1 added a session-match guard to `BrainScheduler::push_continuation`:
//! continuations whose `brain_session` does not match the scheduler's
//! `active_session` are dropped with `DropReason::StaleSession`.
//!
//! Separately, S3.1 wired the MCP server to stamp every detached
//! `BrainContinuation` with `self.brain_session_id.as_session_id().clone()`,
//! where `brain_session_id` comes from `orchestrator::create_brain_session`
//! via `session_id.clone().into()` (i.e., the **SPUR** session id).
//!
//! But the orchestrator was initializing the scheduler's `active_session`
//! via `note_session_swap(Some(SessionId(b.acp_session_id.clone())), ..)` —
//! the **ACP protocol** session id returned by the ACP agent's
//! `new_session()` response (e.g., `stdio_adapter::new_session` generates
//! its own `Uuid::new_v4()`).
//!
//! These two IDs are distinct UUIDs, so every detached continuation was
//! silently dropped and brain agents never received re-prompts for
//! async worker completions.
//!
//! ## What this test pins
//!
//! Given a brain session with **distinct** ACP and SPUR session ids
//! (the real production shape), continuations tagged the way MCP tags
//! them (with `spur_session_id`) must be accepted by a scheduler whose
//! `active_session` is initialized the way the orchestrator initializes
//! it.
//!
//! Pre-fix: scheduler is seeded with `SessionId(acp_session_id)` →
//! continuation is dropped as `StaleSession`.
//! Post-fix: scheduler is seeded with `spur_session_id` →
//! continuation is accepted.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use spur_acp::domain::continuation::DropReason;
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::{new_overflow_buf, report_detached_completion};
use spur_core::event_funnel::spawn_funnel;
use spur_core::orchestrator::InteractiveInput;
use spur_core::scheduler::{BrainScheduler, ScheduledAction};
use tokio::sync::{broadcast, mpsc};

/// Build a continuation the way `spur_mcp::server::build_detached_continuation`
/// builds one: `brain_session` is stamped from the MCP server's
/// `brain_session_id`, which is derived from the SPUR session id.
fn mk_cont_stamped_with(brain_session: &SessionId) -> BrainContinuation {
    BrainContinuation {
        delegation_id: "delegation-regression-1".into(),
        attempt: 1,
        brain_session: brain_session.clone(),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("worker completed".into()),
            diff_summary: None,
            worker_branch: None,
            artifact_ref: None,
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
        },
        created_at_wall: Utc::now(),
        created_at_mono: Instant::now(),
    }
}

/// Drain at most `n` events from the broadcast receiver within `timeout`,
/// returning what arrived.
async fn drain_up_to(
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

/// Happy path: scheduler seeded with `spur_session_id` (post-fix wiring)
/// MUST accept a continuation stamped with `spur_session_id`, even though
/// the simulated brain has a DISTINCT `acp_session_id`.
#[tokio::test(flavor = "current_thread")]
async fn scheduler_seeded_with_spur_session_id_accepts_continuation() {
    // ── Arrange: brain with two distinct session IDs ─────────────────
    //
    // Simulates a real BrainSession where:
    //   spur_session_id = SessionId::new()                          // SPUR-side UUID
    //   acp_session_id  = "acp-returned-xyz-{random}"               // agent-returned ACP ID
    //
    // These NEVER match in production.
    let spur_session_id = SessionId::new();
    let acp_session_id = format!(
        "acp-returned-{}-distinct-from-spur",
        spur_session_id.0.len()
    );
    assert_ne!(
        spur_session_id.0, acp_session_id,
        "precondition: spur_session_id and acp_session_id must be distinct to exercise the bug"
    );

    // ── Arrange: funnel, scheduler, continuation bridge ──────────────
    let (bcast_tx, mut bcast_rx) = broadcast::channel(256);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx, seq);

    let (ingress_tx, mut ingress_rx) = mpsc::channel::<InteractiveInput>(64);
    let overflow = new_overflow_buf();

    // POST-FIX wiring: scheduler is seeded with spur_session_id.
    // (Pre-fix was: SessionId(acp_session_id.clone()).)
    //
    // Post-S3.1+hardening: scheduler now takes `Option<BrainSessionId>`, so
    // passing a raw `SessionId(acp_session_id)` is a compile error. The
    // `.into()` here wraps the SPUR session id as `BrainSessionId`.
    let mut scheduler = BrainScheduler::new(
        Some(spur_session_id.clone().into()),
        Arc::new(funnel.clone()),
    );

    // ── Act: MCP stamps the continuation with spur_session_id (post-S3.1
    // behavior at mcp/server.rs:2389), routes it through the bridge.
    let cont = mk_cont_stamped_with(&spur_session_id);
    let worker_session = SessionId::new();
    report_detached_completion(
        &ingress_tx,
        &overflow,
        spur_session_id.clone(),
        worker_session,
        cont.clone(),
    )
    .await;

    // Orchestrator ingress loop hands SystemContinuation to scheduler.
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

    // ── Assert: no StaleSession drop, scheduler has one pending continuation
    let events = drain_up_to(&mut bcast_rx, 4, Duration::from_millis(150)).await;
    let stale_drops: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.body {
            SpurEventBody::ContinuationDropped {
                reason: DropReason::StaleSession,
                delegation_id,
                ..
            } => Some(delegation_id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        stale_drops.is_empty(),
        "continuation was dropped as StaleSession — the regression this test guards against is live. \
         scheduler.active_session should equal spur_session_id, not acp_session_id. \
         dropped: {stale_drops:?}"
    );

    // Positive confirmation: the scheduler produced a ContinuationPrompt
    // on its next tick, meaning the continuation was accepted and is
    // pending for delivery.
    let action = scheduler.next(Instant::now());
    match action {
        ScheduledAction::ContinuationPrompt(batch) => {
            assert_eq!(
                batch.len(),
                1,
                "expected exactly one continuation to be pending"
            );
        }
        other => panic!(
            "expected ContinuationPrompt (continuation was accepted), got {other:?} \
             (implies the continuation was dropped upstream)",
        ),
    }
}

/// Pre-fix reproduction: scheduler seeded with `SessionId(acp_session_id)`
/// (the broken pre-fix wiring) MUST drop a continuation stamped with
/// `spur_session_id` as `StaleSession`. Documents the failure mode so
/// a future refactor that accidentally reverts the fix fails loudly.
#[tokio::test(flavor = "current_thread")]
async fn scheduler_seeded_with_acp_session_id_drops_continuation_as_stale() {
    let spur_session_id = SessionId::new();
    let acp_session_id = format!(
        "acp-returned-{}-distinct-from-spur",
        spur_session_id.0.len()
    );

    let (bcast_tx, mut bcast_rx) = broadcast::channel(256);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx, seq);

    let (ingress_tx, mut ingress_rx) = mpsc::channel::<InteractiveInput>(64);
    let overflow = new_overflow_buf();

    // PRE-FIX wiring: scheduler is seeded with SessionId(acp_session_id).
    //
    // Note: with the hardening commit, `BrainScheduler::new` takes
    // `Option<BrainSessionId>` — so in real code you would need an explicit
    // `.into()` here. This test threads the acp id through the same newtype
    // wrapper to REPRODUCE the pre-fix bug: even after the newtype guard,
    // if a caller mechanically wraps `SessionId(acp_session_id)` as
    // `BrainSessionId`, the continuation still drops because the inner
    // string doesn't match `spur_session_id`. That failure mode is what
    // the docstring invariant on `note_session_swap` warns against.
    let mut scheduler = BrainScheduler::new(
        Some(SessionId(acp_session_id.clone()).into()),
        Arc::new(funnel.clone()),
    );

    let cont = mk_cont_stamped_with(&spur_session_id);
    let worker_session = SessionId::new();
    report_detached_completion(
        &ingress_tx,
        &overflow,
        spur_session_id.clone(),
        worker_session,
        cont.clone(),
    )
    .await;

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

    // Assert the bug: exactly one StaleSession drop for our delegation.
    let events = drain_up_to(&mut bcast_rx, 4, Duration::from_millis(150)).await;
    let stale_drops: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.body {
            SpurEventBody::ContinuationDropped {
                reason: DropReason::StaleSession,
                delegation_id,
                ..
            } => Some(delegation_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        stale_drops.len(),
        1,
        "expected the broken wiring to drop the continuation as StaleSession, got drops: {stale_drops:?}"
    );

    // And confirm nothing is pending for the scheduler.
    let action = scheduler.next(Instant::now());
    match action {
        ScheduledAction::IdleUntil { .. } => {}
        other => panic!("expected IdleUntil (no pending continuation), got {other:?}"),
    }
}
