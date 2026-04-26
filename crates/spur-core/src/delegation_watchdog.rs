use spur_acp::config::WorktreeConfig;
use spur_acp::{
    DelegationAbortHandle, DelegationAbortReason, DelegationStatus, SpurEvent, SpurEventBody,
};
use tokio::sync::{broadcast, oneshot};
use tokio::time::{Duration, Instant};

pub type HeartbeatWatchdogStop = oneshot::Sender<()>;

pub async fn status_from_abort_reason(handle: &DelegationAbortHandle) -> DelegationStatus {
    match handle.observed_reason().await {
        Some(DelegationAbortReason::BrainRequested { reason }) => {
            DelegationStatus::Cancelled { reason }
        }
        Some(DelegationAbortReason::WorkerHeartbeatTimeout {
            executor_id: _,
            idle_for_secs: _,
        }) => DelegationStatus::Timeout,
        None => {
            tracing::warn!(
                "cancel_token cancelled without DelegationAbortReason - caller bypassed request_abort"
            );
            DelegationStatus::Cancelled {
                reason: "brain requested cancel".into(),
            }
        }
    }
}

pub fn maybe_spawn_heartbeat_watchdog(
    config: &WorktreeConfig,
    request_id: String,
    abort_handle: DelegationAbortHandle,
    event_tx: &broadcast::Sender<SpurEvent>,
) -> Option<HeartbeatWatchdogStop> {
    if !config.worker_heartbeat_watchdog_enabled {
        return None;
    }

    let event_rx = event_tx.subscribe();
    let (stop_tx, stop_rx) = oneshot::channel();
    let timeout_secs = config.worker_heartbeat_timeout_secs;
    let initial_grace_secs = config.worker_heartbeat_initial_grace_secs;
    tracing::info!(
        request_id = %request_id,
        timeout_secs,
        initial_grace_secs,
        "heartbeat watchdog spawned"
    );
    tokio::spawn(run_heartbeat_watchdog(
        request_id,
        abort_handle,
        event_rx,
        stop_rx,
        timeout_secs,
        initial_grace_secs,
    ));
    Some(stop_tx)
}

pub async fn run_heartbeat_watchdog(
    request_id: String,
    abort_handle: DelegationAbortHandle,
    mut event_rx: broadcast::Receiver<SpurEvent>,
    mut stop_rx: oneshot::Receiver<()>,
    timeout_secs: u64,
    initial_grace_secs: u64,
) {
    let steady_timeout = Duration::from_secs(timeout_secs);
    let initial_grace = Duration::from_secs(initial_grace_secs.max(timeout_secs.saturating_mul(2)));
    let mut deadline = Instant::now() + initial_grace;
    let mut executor_id: Option<String> = None;

    loop {
        tokio::select! {
            biased;

            _ = &mut stop_rx => {
                return;
            }

            recv = event_rx.recv() => {
                match recv {
                    Ok(event) => match &event.body {
                        SpurEventBody::DelegationDispatched {
                            request_id: dispatched_request_id,
                            executor_id: dispatched_executor_id,
                            ..
                        } if dispatched_request_id == &request_id => {
                            executor_id = Some(dispatched_executor_id.clone());
                            deadline = Instant::now() + steady_timeout;
                        }
                        SpurEventBody::WorkerHeartbeat {
                            executor_id: heartbeat_executor_id,
                            ..
                        } if executor_id.as_deref() == Some(heartbeat_executor_id.as_str()) => {
                            deadline = Instant::now() + steady_timeout;
                        }
                        _ => {}
                    },
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            request_id = %request_id,
                            lagged = n,
                            "heartbeat watchdog lagged broadcast; not treating missed messages as liveness"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }

            _ = tokio::time::sleep_until(deadline) => {
                let (executor_id, idle_for_secs) = match executor_id {
                    Some(executor_id) => (executor_id, steady_timeout.as_secs()),
                    None => ("<not-dispatched>".into(), initial_grace.as_secs()),
                };
                tracing::warn!(
                    request_id = %request_id,
                    executor_id = %executor_id,
                    idle_for_secs,
                    "heartbeat watchdog timeout fired"
                );
                abort_handle
                    .request_abort(DelegationAbortReason::WorkerHeartbeatTimeout {
                        executor_id,
                        idle_for_secs,
                    })
                    .await;
                return;
            }
        }
    }
}
