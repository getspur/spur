// The `Send` proof for spawned server futures traverses deep dependency
// type chains (lance_io/moka/portable_atomic) inside spur-context; the
// chain exceeds the default trait-solver recursion limit (E0275).
#![recursion_limit = "256"]

use std::sync::Arc;
use std::time::Duration;

use spur_acp::config::WorktreeConfig;
use spur_acp::types::SessionId;
use spur_acp::{
    CancellationControl, DelegationAbortReason, DelegationStatus, SpurEvent, SpurEventBody,
};
use spur_core::delegation_watchdog::{
    maybe_spawn_heartbeat_watchdog, run_heartbeat_watchdog, status_from_abort_reason,
};
use tokio::sync::{broadcast, oneshot, Semaphore};

fn dispatched(request_id: &str, executor_id: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain".into()),
        request_id: request_id.into(),
        executor_id: executor_id.into(),
    })
}

fn heartbeat(executor_id: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::WorkerHeartbeat {
        brain_session_id: SessionId("brain".into()),
        executor_id: executor_id.into(),
        worker_ts: None,
    })
}

async fn yield_to_watchdog() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancel_during_permit_wait_short_circuits() {
    let semaphore = Arc::new(Semaphore::new(1));
    let held = semaphore.clone().acquire_owned().await.unwrap();
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc.register_with_abort_handle("request-queued".into()).await;

    let waiter_semaphore = semaphore.clone();
    let waiter_abort_handle = abort_handle.clone();
    let waiter = tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = waiter_abort_handle.cancelled() => {
                Err(status_from_abort_reason(&waiter_abort_handle).await)
            }
            permit = waiter_semaphore.acquire_owned() => {
                Ok(permit.expect("semaphore stays open"))
            }
        }
    });

    yield_to_watchdog().await;
    let outcome = cc
        .cancel_with_reason("request-queued", "brain changed plan".into())
        .await;
    assert_eq!(outcome, spur_acp::CancelOutcome::Cancelled);
    let status = waiter.await.expect("waiter task").expect_err("cancel wins");
    assert_eq!(
        status,
        DelegationStatus::Cancelled {
            reason: "brain changed plan".into()
        }
    );
    drop(held);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn watchdog_disabled_by_default_no_spawn() {
    let cfg = WorktreeConfig::default();
    assert!(!cfg.worker_heartbeat_watchdog_enabled);

    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc
        .register_with_abort_handle("request-default".into())
        .await;
    let (event_tx, _event_rx) = broadcast::channel(8);

    let stop = maybe_spawn_heartbeat_watchdog(
        &cfg,
        "request-default".into(),
        abort_handle.clone(),
        &event_tx,
    );
    assert!(stop.is_none());

    tokio::time::advance(Duration::from_secs(10_000)).await;
    assert_eq!(abort_handle.observed_reason().await, None);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn silent_worker_triggers_watchdog_timeout() {
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc.register_with_abort_handle("request-silent".into()).await;
    let (event_tx, event_rx) = broadcast::channel(8);
    let (_stop_tx, stop_rx) = oneshot::channel();

    tokio::spawn(run_heartbeat_watchdog(
        "request-silent".into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        5,
        1,
    ));
    event_tx
        .send(dispatched("request-silent", "exec-silent"))
        .unwrap();
    yield_to_watchdog().await;

    tokio::time::advance(Duration::from_secs(5)).await;
    yield_to_watchdog().await;

    assert_eq!(
        abort_handle.observed_reason().await,
        Some(DelegationAbortReason::WorkerHeartbeatTimeout {
            executor_id: "exec-silent".into(),
            idle_for_secs: 5,
        })
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn heartbeating_worker_survives_indefinitely() {
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc.register_with_abort_handle("request-alive".into()).await;
    let (event_tx, event_rx) = broadcast::channel(16);
    let (stop_tx, stop_rx) = oneshot::channel();

    tokio::spawn(run_heartbeat_watchdog(
        "request-alive".into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        5,
        1,
    ));
    event_tx
        .send(dispatched("request-alive", "exec-alive"))
        .unwrap();
    yield_to_watchdog().await;

    for _ in 0..20 {
        tokio::time::advance(Duration::from_secs(4)).await;
        event_tx.send(heartbeat("exec-alive")).unwrap();
        yield_to_watchdog().await;
        assert_eq!(abort_handle.observed_reason().await, None);
    }

    drop(stop_tx);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn initial_grace_period_covers_startup() {
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc
        .register_with_abort_handle("request-startup".into())
        .await;
    let (event_tx, event_rx) = broadcast::channel(8);
    let (_stop_tx, stop_rx) = oneshot::channel();

    tokio::spawn(run_heartbeat_watchdog(
        "request-startup".into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        5,
        1,
    ));

    tokio::time::advance(Duration::from_secs(9)).await;
    yield_to_watchdog().await;
    assert_eq!(abort_handle.observed_reason().await, None);

    event_tx
        .send(dispatched("request-startup", "exec-startup"))
        .unwrap();
    yield_to_watchdog().await;
    assert_eq!(abort_handle.observed_reason().await, None);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn brain_cancel_preempts_watchdog() {
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc.register_with_abort_handle("request-cancel".into()).await;
    let (event_tx, event_rx) = broadcast::channel(8);
    let (_stop_tx, stop_rx) = oneshot::channel();

    tokio::spawn(run_heartbeat_watchdog(
        "request-cancel".into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        5,
        1,
    ));
    event_tx
        .send(dispatched("request-cancel", "exec-cancel"))
        .unwrap();
    yield_to_watchdog().await;

    abort_handle
        .request_abort(DelegationAbortReason::BrainRequested {
            reason: "operator cancelled".into(),
        })
        .await;
    tokio::time::advance(Duration::from_secs(5)).await;
    yield_to_watchdog().await;

    assert_eq!(
        abort_handle.observed_reason().await,
        Some(DelegationAbortReason::BrainRequested {
            reason: "operator cancelled".into(),
        })
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn lagged_broadcast_does_not_reset_deadline() {
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc.register_with_abort_handle("request-lagged".into()).await;
    let (event_tx, event_rx) = broadcast::channel(1);
    let (_stop_tx, stop_rx) = oneshot::channel();

    tokio::spawn(run_heartbeat_watchdog(
        "request-lagged".into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        5,
        1,
    ));
    event_tx
        .send(dispatched("request-lagged", "exec-lagged"))
        .unwrap();
    yield_to_watchdog().await;

    event_tx.send(heartbeat("other-exec")).unwrap();
    event_tx.send(heartbeat("other-exec")).unwrap();
    event_tx.send(heartbeat("other-exec")).unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    yield_to_watchdog().await;

    assert_eq!(
        abort_handle.observed_reason().await,
        Some(DelegationAbortReason::WorkerHeartbeatTimeout {
            executor_id: "exec-lagged".into(),
            idle_for_secs: 5,
        })
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn normal_completion_stops_watchdog_cleanly() {
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc.register_with_abort_handle("request-done".into()).await;
    let (event_tx, event_rx) = broadcast::channel(8);
    let (stop_tx, stop_rx) = oneshot::channel();

    let join = tokio::spawn(run_heartbeat_watchdog(
        "request-done".into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        5,
        1,
    ));
    event_tx
        .send(dispatched("request-done", "exec-done"))
        .unwrap();
    yield_to_watchdog().await;

    drop(stop_tx);
    join.await.expect("watchdog exits cleanly");
    tokio::time::advance(Duration::from_secs(100)).await;
    assert_eq!(abort_handle.observed_reason().await, None);
}
