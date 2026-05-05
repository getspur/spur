//! INV-6: CancellationControl primitive tests and DelegationCompleted
//! emission regression test.
//!
//! A full Orchestrator e2e cancellation test would require a real git
//! worktree and a fake ACP worker, which is out of scope for this pass.
//! Instead we test the `CancellationControl` primitive — the token
//! registry used by `handle_delegations`'s `tokio::select!` — in
//! isolation.  This proves the mechanism works; an Orchestrator-level
//! integration test can be added later.

use spur_acp::{
    CancelOutcome, CancellationControl, DelegationResult, DelegationStatus, SpurEventBody,
};
use std::sync::Arc;
use std::time::Duration;

/// Register a token, race execute_delegation look-alike against it,
/// signal cancel — verify the select! arm yields Cancelled status.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancel_wins_over_slow_worker() {
    let cc = CancellationControl::new();

    // Simulate what handle_delegations does: register token before spawning.
    let token = cc.register("delegation-abc".into()).await;

    // Spawn a "slow worker" task that returns only after 60 virtual seconds.
    let worker_handle = tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = token.cancelled() => {
                DelegationResult {
                    status: DelegationStatus::Cancelled {
                        reason: "brain requested cancel".into(),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                DelegationResult {
                    status: DelegationStatus::Success,
                    diff: None,
                    diff_summary: None,
                    summary: Some("completed normally".into()),
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                }
            }
        };
        result
    });

    // Yield so the spawned task is polled and enters select!.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // Cancel before 60s elapses — should win the select!.
    let outcome = cc.cancel("delegation-abc").await;
    assert_eq!(outcome, CancelOutcome::Cancelled);

    // Advance virtual time slightly (cancel should already have fired).
    tokio::time::advance(Duration::from_millis(100)).await;

    let result = worker_handle.await.expect("worker task panicked");
    assert!(
        matches!(result.status, DelegationStatus::Cancelled { .. }),
        "expected Cancelled status, got {:?}",
        result.status
    );

    // Token entry was removed by cancel() — a second call is NotFound.
    let second = cc.cancel("delegation-abc").await;
    assert_eq!(second, CancelOutcome::NotFound);
}

/// Regression: the cancel arm of `tokio::select!` must emit
/// `DelegationCompleted` so the TUI / lineage projection never see a
/// delegation stuck "active" forever.
///
/// We mirror the production cancel arm exactly (token + funnel emit +
/// select! against a slow future) using a real `spawn_funnel` and a
/// broadcast receiver — same pattern used in `event_funnel` unit tests.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancel_emits_delegation_completed() {
    use spur_acp::types::SessionId;
    use spur_core::event_funnel::spawn_funnel;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::broadcast;

    // Set up a real funnel + broadcast subscriber.
    let (bcast_tx, mut bcast_rx) = broadcast::channel(32);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx, seq);

    let cc = CancellationControl::new();
    let token = cc.register("del-reg".into()).await;
    let funnel_for_task = funnel.clone();

    // Mirror the production cancel arm: select! cancel_token vs slow work,
    // emitting DelegationCompleted on the cancel branch.
    let worker = tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = token.cancelled() => {
                let status = DelegationStatus::Cancelled {
                    reason: "brain requested cancel".into(),
                };
                funnel_for_task.emit(SpurEventBody::DelegationCompleted {
                    worker_session: SessionId("del-reg".into()),
                    status: status.clone(),
                });
                DelegationResult {
                    status,
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                DelegationResult {
                    status: DelegationStatus::Success,
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                }
            }
        };
        result
    });

    // Yield so the worker task enters select!.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // Signal cancel.
    let outcome = cc.cancel("del-reg").await;
    assert_eq!(outcome, CancelOutcome::Cancelled);

    // Advance virtual time to let the funnel task drain the channel.
    tokio::time::advance(Duration::from_millis(10)).await;

    // Worker must complete with Cancelled.
    let result = worker.await.expect("worker panicked");
    assert!(
        matches!(result.status, DelegationStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );

    // The broadcast must contain a DelegationCompleted(Cancelled) event.
    // Yield a few times so the funnel task processes the queued emit.
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }

    let event = bcast_rx
        .try_recv()
        .expect("DelegationCompleted must be on the broadcast channel");
    assert!(
        matches!(
            event.body,
            SpurEventBody::DelegationCompleted {
                status: DelegationStatus::Cancelled { .. },
                ..
            }
        ),
        "expected DelegationCompleted(Cancelled), got {:?}",
        event.body
    );
}

/// INV-ASYNC-3, Sub-case 1: cancel arriving *during the inline window* must
/// reach the orchestrator cancellation-control token; the fast (oneshot) arm
/// of `handle_delegate_to_worker` then wins and the brain sees the Cancelled
/// result inline. No detached collector must run.
///
/// Harness deliberately constructs `McpCallbackServer` directly and drives
/// the handler via `__test_call_delegate_to_worker` + the fake worker reads
/// the `DelegationChannel` in-process. No HTTP transport, no rmcp session
/// handshake — those race against `tokio::time::pause()` and caused the
/// prior attempt at this test to flake.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_cancel_during_inline_window_fast_arm_wins() {
    use serde_json::Value;
    use spur_acp::domain::ContinuationSource;
    use spur_acp::types::SessionId;
    use spur_acp::{BrainSessionId, CancellationControl, DelegationResult, DelegationStatus};
    use spur_mcp::server::DetachedContinuationCtx;
    use spur_mcp::{McpCallbackServer, WorkerInfo};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::time::Duration;

    let callback_count = Arc::new(AtomicUsize::new(0));
    let cancelled_source_count = Arc::new(AtomicUsize::new(0));
    let cb = Arc::clone(&callback_count);
    let cb_cancelled = Arc::clone(&cancelled_source_count);
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(move |cont, _worker| {
            let cb = Arc::clone(&cb);
            let cb_cancelled = Arc::clone(&cb_cancelled);
            Box::pin(async move {
                cb.fetch_add(1, Ordering::SeqCst);
                if matches!(cont.source, ContinuationSource::Cancelled) {
                    cb_cancelled.fetch_add(1, Ordering::SeqCst);
                }
            })
        }),
    };

    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        spur_mcp::server::community_feature_gate(),
    );
    server.set_workers(vec![WorkerInfo {
        name: "worker-x".into(),
        tier: Some("generalist".into()),
        ..Default::default()
    }]);
    server.set_inline_wait(Duration::from_secs(5));
    let cc = CancellationControl::new();
    server.set_cancellation_control(cc.clone());
    let server = Arc::new(server);

    // Fake worker: recv request → register cancel token → publish id → select
    // on cancelled vs a 600s sleep → send Cancelled on the oneshot.
    //
    // Token is registered *before* `id_tx.send`, so the test-side code that
    // awaits `id_rx` is guaranteed `cc.cancel(id)` will find a live token.
    let (id_tx, id_rx) = tokio::sync::oneshot::channel::<String>();
    let cc_worker = cc.clone();
    let worker_handle = tokio::spawn(async move {
        let req = channel.request_rx.recv().await.expect("delegation request");
        let delegation_id: String = req.id.clone().into();
        let token = cc_worker.register(delegation_id.clone()).await;
        let _ = id_tx.send(delegation_id.clone());
        let status = tokio::select! {
            biased;
            _ = token.cancelled() => DelegationStatus::Cancelled {
                reason: "brain requested cancel".into(),
            },
            _ = tokio::time::sleep(Duration::from_secs(600)) => DelegationStatus::Success,
        };
        let _ = req.respond_to.send(DelegationResult {
            status,
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        });
        delegation_id
    });

    let server_ref = Arc::clone(&server);
    let mcp_resp_handle = tokio::spawn(async move {
        server_ref
            .__test_call_delegate_to_worker("worker-x", "cancel-me")
            .await
    });

    let delegation_id = id_rx.await.expect("worker must publish delegation id");

    // t = 2.5s, still inside 5s inline window — handler's slow arm has not
    // fired; no collector spawned yet.
    tokio::time::advance(Duration::from_millis(2_500)).await;

    // Cancel. The response body is not the assertion target (worker-observed
    // behaviour through the inline arm is), but we sanity-check it's a
    // well-formed JSON-RPC success response.
    let cancel_resp = server.__test_call_cancel_delegation(&delegation_id).await;
    assert!(
        cancel_resp.pointer("/result").is_some(),
        "cancel response must be a JSON-RPC success, got {cancel_resp:?}",
    );

    let mcp_resp = mcp_resp_handle.await.expect("handler join");
    let _ = worker_handle.await.expect("worker join");

    // Drain any collector spawned via the slow path (there should be none on
    // the fast arm, but this also closes the TaskTracker cleanly).
    server.shutdown().await;

    let text = mcp_resp
        .pointer("/result/content/0/text")
        .and_then(|t| t.as_str())
        .expect("mcp response must carry content[0].text");
    let payload: Value = serde_json::from_str(text).expect("payload is JSON");

    assert_eq!(
        payload["status"].as_str(),
        Some("completed"),
        "INV-ASYNC-3 fast arm: handler must report status=completed even on \
         cancel; payload={payload:#}",
    );
    assert!(
        payload["result"]["status"].get("Cancelled").is_some(),
        "INV-ASYNC-3 fast arm: embedded DelegationResult status must be \
         Cancelled; payload={payload:#}",
    );

    // Fast arm must not spawn a detached collector (no callback, no map).
    assert_eq!(
        callback_count.load(Ordering::SeqCst),
        0,
        "fast arm must NOT fire a detached continuation callback",
    );
    assert_eq!(
        cancelled_source_count.load(Ordering::SeqCst),
        0,
        "fast arm must NOT fire ContinuationSource::Cancelled",
    );
    assert!(
        !server.__test_completed_has(&delegation_id).await,
        "fast arm must NOT leave a completed_delegations entry",
    );
}

/// INV-ASYNC-3, Sub-case 2: cancel arriving *after* the inline window has
/// elapsed — handler's slow arm has already detached the oneshot into a
/// collector. The cancel must still reach the orchestrator cancellation-control
/// token, the worker must resolve with `Cancelled`, and the collector must
/// deliver that outcome via the continuation callback with
/// `ContinuationSource::Cancelled`. Per INV-ASYNC-2 (rev 2) the BlockTimeout
/// collector skips the `completed_delegations` map write, so no leftover
/// entry may remain.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_cancel_during_detached_path_continuation_delivers_cancelled() {
    use serde_json::Value;
    use spur_acp::domain::ContinuationSource;
    use spur_acp::types::SessionId;
    use spur_acp::{BrainSessionId, CancellationControl, DelegationResult, DelegationStatus};
    use spur_mcp::server::DetachedContinuationCtx;
    use spur_mcp::{McpCallbackServer, WorkerInfo};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::time::Duration;

    let callback_count = Arc::new(AtomicUsize::new(0));
    let cancelled_source_count = Arc::new(AtomicUsize::new(0));
    let cb = Arc::clone(&callback_count);
    let cb_cancelled = Arc::clone(&cancelled_source_count);
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(move |cont, _worker| {
            let cb = Arc::clone(&cb);
            let cb_cancelled = Arc::clone(&cb_cancelled);
            Box::pin(async move {
                cb.fetch_add(1, Ordering::SeqCst);
                if matches!(cont.source, ContinuationSource::Cancelled) {
                    cb_cancelled.fetch_add(1, Ordering::SeqCst);
                }
            })
        }),
    };

    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        spur_mcp::server::community_feature_gate(),
    );
    server.set_workers(vec![WorkerInfo {
        name: "worker-x".into(),
        tier: Some("generalist".into()),
        ..Default::default()
    }]);
    server.set_inline_wait(Duration::from_secs(5));
    let cc = CancellationControl::new();
    server.set_cancellation_control(cc.clone());
    let server = Arc::new(server);

    let (id_tx, id_rx) = tokio::sync::oneshot::channel::<String>();
    let cc_worker = cc.clone();
    let worker_handle = tokio::spawn(async move {
        let req = channel.request_rx.recv().await.expect("delegation request");
        let delegation_id: String = req.id.clone().into();
        let token = cc_worker.register(delegation_id.clone()).await;
        let _ = id_tx.send(delegation_id.clone());
        let status = tokio::select! {
            biased;
            _ = token.cancelled() => DelegationStatus::Cancelled {
                reason: "brain requested cancel".into(),
            },
            _ = tokio::time::sleep(Duration::from_secs(600)) => DelegationStatus::Success,
        };
        let _ = req.respond_to.send(DelegationResult {
            status,
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        });
        delegation_id
    });

    let server_ref = Arc::clone(&server);
    let mcp_resp_handle = tokio::spawn(async move {
        server_ref
            .__test_call_delegate_to_worker("worker-x", "cancel-me-detached")
            .await
    });

    let delegation_id = id_rx.await.expect("worker must publish delegation id");

    // t = 6s, past the 5s inline window — handler's slow arm wins, a collector
    // has been spawned and handler returned "pending". Worker is still parked
    // in its own select! (600s sleep has not fired).
    tokio::time::advance(Duration::from_millis(6_000)).await;

    let mcp_resp = mcp_resp_handle.await.expect("handler join");
    let text = mcp_resp
        .pointer("/result/content/0/text")
        .and_then(|t| t.as_str())
        .expect("mcp response must carry content[0].text");
    let payload: Value = serde_json::from_str(text).expect("payload is JSON");
    assert_eq!(
        payload["status"].as_str(),
        Some("pending"),
        "slow arm must report status=pending; payload={payload:#}",
    );
    assert_eq!(
        payload["continuation_will_fire"].as_bool(),
        Some(true),
        "slow arm must declare a continuation will fire; payload={payload:#}",
    );

    // Cancel AFTER detachment — reaches the token, worker resolves Cancelled,
    // collector fires the continuation callback.
    let cancel_resp = server.__test_call_cancel_delegation(&delegation_id).await;
    assert!(
        cancel_resp.pointer("/result").is_some(),
        "cancel response must be a JSON-RPC success, got {cancel_resp:?}",
    );

    let _ = worker_handle.await.expect("worker join");

    // Close the TaskTracker and wait for the collector to drain the oneshot,
    // run its post-completion writes, and invoke the continuation callback.
    server.shutdown().await;

    // INV-ASYNC-3: continuation callback fires exactly once with
    // ContinuationSource::Cancelled.
    assert_eq!(
        callback_count.load(Ordering::SeqCst),
        1,
        "collector must fire continuation callback exactly once on cancel",
    );
    assert_eq!(
        cancelled_source_count.load(Ordering::SeqCst),
        1,
        "continuation source must be ContinuationSource::Cancelled",
    );

    // INV-ASYNC-2 (rev 2): BlockTimeout collector skips the map write. Leaving
    // an entry here would let a later `check_delegation_status` double-deliver
    // the same result.
    assert!(
        !server.__test_completed_has(&delegation_id).await,
        "BlockTimeout cancel path must NOT leave a completed_delegations entry",
    );
}

/// Normal completion removes the token so a subsequent cancel call is NotFound.
#[tokio::test(flavor = "current_thread")]
async fn normal_completion_removes_token() {
    let cc = CancellationControl::new();
    cc.register("delegation-xyz".into()).await;

    // Simulate normal task completion: remove token without cancelling.
    cc.remove("delegation-xyz").await;

    // After normal completion, cancel returns NotFound (already cleaned up).
    let outcome = cc.cancel("delegation-xyz").await;
    assert_eq!(outcome, CancelOutcome::NotFound);
}
