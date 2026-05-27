#![cfg(madsim)]
#![allow(unexpected_cfgs)]

extern crate madsim_tokio as tokio;

#[allow(dead_code)]
#[path = "../src/delegation_watchdog.rs"]
mod delegation_watchdog;

use std::time::Duration;

use spur_acp::types::SessionId;
use spur_acp::{CancellationControl, DelegationAbortReason, SpurEvent, SpurEventBody};
use tokio::sync::{broadcast, oneshot};

const REQUEST_ID: &str = "request-madsim";
const EXECUTOR_ID: &str = "exec-madsim";
const STEADY_TIMEOUT: Duration = Duration::from_secs(1);
const INITIAL_GRACE: Duration = Duration::from_secs(2);
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(900);
const DISPATCH_AT: Duration = Duration::from_millis(1_900);

#[test]
fn heartbeat_watchdog_madsim_schedules() {
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_from_env();
        no_heartbeat_escalates_at_initial_grace().await;
        heartbeats_before_steady_timeout_prevent_escalation().await;
        escalation_follows_last_heartbeat_plus_steady_timeout().await;
    });
}

async fn no_heartbeat_escalates_at_initial_grace() {
    let (_event_tx, event_rx) = broadcast::channel(8);
    let (_stop_tx, stop_rx) = oneshot::channel();
    let abort_handle = abort_handle(REQUEST_ID).await;

    tokio::spawn(delegation_watchdog::run_heartbeat_watchdog(
        REQUEST_ID.into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        1,
        2,
    ));

    tokio::time::sleep(INITIAL_GRACE - Duration::from_millis(1)).await;
    yield_to_watchdog().await;
    assert_eq!(abort_handle.observed_reason().await, None);

    tokio::time::sleep(Duration::from_millis(2)).await;
    yield_to_watchdog().await;
    assert_eq!(
        abort_handle.observed_reason().await,
        Some(DelegationAbortReason::WorkerHeartbeatTimeout {
            executor_id: "<not-dispatched>".into(),
            idle_for_secs: INITIAL_GRACE.as_secs(),
        })
    );
}

async fn heartbeats_before_steady_timeout_prevent_escalation() {
    let (event_tx, event_rx) = broadcast::channel(16);
    let (stop_tx, stop_rx) = oneshot::channel();
    let abort_handle = abort_handle(REQUEST_ID).await;

    let join = tokio::spawn(delegation_watchdog::run_heartbeat_watchdog(
        REQUEST_ID.into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        1,
        2,
    ));

    tokio::time::sleep(DISPATCH_AT).await;
    event_tx.send(dispatched()).unwrap();
    yield_to_watchdog().await;

    for _ in 0..6 {
        tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        event_tx.send(heartbeat()).unwrap();
        yield_to_watchdog().await;
        assert_eq!(abort_handle.observed_reason().await, None);
    }

    tokio::time::sleep(STEADY_TIMEOUT - Duration::from_millis(50)).await;
    yield_to_watchdog().await;
    assert_eq!(abort_handle.observed_reason().await, None);

    drop(stop_tx);
    join.await.expect("watchdog exits after stop");
}

async fn escalation_follows_last_heartbeat_plus_steady_timeout() {
    let (event_tx, event_rx) = broadcast::channel(16);
    let (_stop_tx, stop_rx) = oneshot::channel();
    let abort_handle = abort_handle(REQUEST_ID).await;

    tokio::spawn(delegation_watchdog::run_heartbeat_watchdog(
        REQUEST_ID.into(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        1,
        2,
    ));

    tokio::time::sleep(DISPATCH_AT).await;
    event_tx.send(dispatched()).unwrap();
    yield_to_watchdog().await;

    tokio::time::sleep(HEARTBEAT_INTERVAL).await;
    event_tx.send(heartbeat()).unwrap();
    yield_to_watchdog().await;
    assert_eq!(abort_handle.observed_reason().await, None);

    tokio::time::sleep(HEARTBEAT_INTERVAL).await;
    event_tx.send(heartbeat()).unwrap();
    let last_heartbeat_at = tokio::time::Instant::now();
    yield_to_watchdog().await;
    assert_eq!(abort_handle.observed_reason().await, None);

    tokio::time::sleep(STEADY_TIMEOUT - Duration::from_millis(1)).await;
    yield_to_watchdog().await;
    assert_eq!(abort_handle.observed_reason().await, None);

    tokio::time::sleep(Duration::from_millis(2)).await;
    yield_to_watchdog().await;
    assert!(
        tokio::time::Instant::now().duration_since(last_heartbeat_at) >= STEADY_TIMEOUT,
        "watchdog should not escalate before the steady timeout elapses"
    );
    assert_eq!(
        abort_handle.observed_reason().await,
        Some(DelegationAbortReason::WorkerHeartbeatTimeout {
            executor_id: EXECUTOR_ID.into(),
            idle_for_secs: STEADY_TIMEOUT.as_secs(),
        })
    );
}

async fn abort_handle(request_id: &str) -> spur_acp::DelegationAbortHandle {
    let cc = CancellationControl::new();
    let (_token, abort_handle) = cc.register_with_abort_handle(request_id.into()).await;
    abort_handle
}

fn dispatched() -> SpurEvent {
    SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain".into()),
        request_id: REQUEST_ID.into(),
        executor_id: EXECUTOR_ID.into(),
    })
}

fn heartbeat() -> SpurEvent {
    SpurEvent::now(SpurEventBody::WorkerHeartbeat {
        brain_session_id: SessionId("brain".into()),
        executor_id: EXECUTOR_ID.into(),
        worker_ts: None,
    })
}

async fn yield_to_watchdog() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

fn assert_seed_from_env() {
    if let Ok(seed) = std::env::var("MADSIM_TEST_SEED") {
        assert_eq!(
            madsim::runtime::Handle::current().seed(),
            seed.parse::<u64>().expect("MADSIM_TEST_SEED must be u64"),
        );
    }
}
