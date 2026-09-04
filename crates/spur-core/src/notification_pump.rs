//! Fan-out pump: drains a connection-scoped broadcast of SessionNotifications
//! and emits SpurEventBody::AgentNotification onto the SpurEvent bus via the
//! funnel, tagged with the caller-supplied spur_session_id.
//!
//! **Coupled invariant with `AgentConnection::prompt()`:** to avoid double-
//! emitting `AgentNotification` on the event bus, any transport whose
//! `subscribe_session_notifications()` returns `Some(...)` (i.e. participates
//! in this pump) MUST also return an empty `Stream` from `prompt()` and
//! `load_session()`. Transports that stream notifications synchronously
//! (stdio, cli_wrap, stream_json) return `None` from subscribe and let the
//! brain's inline stream drain at `orchestrator.rs` handle emission. Only
//! `NativeAcpConnection` currently participates.
//!
//! Terminal milestones use an observed pump rather than sleeping and hoping
//! the pump wins the scheduler race. After the established grace interval,
//! the owner sends a barrier command. The pump drains every notification that
//! is already queued, enqueues the corresponding events onto the same funnel
//! as the milestone, and only then acknowledges the barrier. Funnel FIFO order
//! therefore makes the notification events precede the terminal milestone.

use std::time::Duration;

use tokio::sync::broadcast::{error::RecvError, error::TryRecvError, Receiver};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use spur_acp::connection::{AgentClientRequestKind, AgentClientRequestPayload};
use spur_acp::domain::events::SpurEventBody;
use spur_acp::types::SessionId;
use spur_acp::SessionNotification;

use crate::event_funnel::FunnelHandle;

pub(crate) const TRAILING_NOTIFICATION_GRACE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NotificationPumpSnapshot {
    pub(crate) emitted: u64,
}

type BarrierReply = oneshot::Sender<NotificationPumpSnapshot>;

/// Owned notification pump for a live brain session.
///
/// In addition to aborting the background task, this handle can establish a
/// FIFO barrier between queued notification events and a later terminal event.
/// Dropping the handle aborts the task so a retired session cannot keep
/// emitting events under its stale session id.
pub struct SessionNotificationPump {
    task: Option<JoinHandle<()>>,
    barrier_tx: mpsc::UnboundedSender<BarrierReply>,
}

impl SessionNotificationPump {
    /// Abort the background notification task.
    pub fn abort(&self) {
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }

    /// Wait for the trailing-notification grace interval, then flush every
    /// broadcast item queued at the barrier before returning its counters.
    pub(crate) async fn settle_after_terminal(&self) -> NotificationPumpSnapshot {
        tokio::time::sleep(TRAILING_NOTIFICATION_GRACE).await;

        let (reply_tx, reply_rx) = oneshot::channel();
        if self.barrier_tx.send(reply_tx).is_err() {
            return NotificationPumpSnapshot::default();
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Retire while retaining the taken task handle across force escalation.
    /// Returns `true` when `force_shutdown` won the graceful wait.
    pub(crate) async fn retire_until_forced(mut self, force_shutdown: &CancellationToken) -> bool {
        let Some(mut task) = self.task.take() else {
            return force_shutdown.is_cancelled();
        };

        if force_shutdown.is_cancelled() {
            task.abort();
            let _ = task.await;
            return true;
        }

        tokio::select! {
            biased;
            _ = force_shutdown.cancelled() => {
                task.abort();
                let _ = task.await;
                true
            }
            _ = tokio::time::sleep(TRAILING_NOTIFICATION_GRACE) => {
                task.abort();
                let _ = task.await;
                force_shutdown.is_cancelled()
            }
            _ = &mut task => force_shutdown.is_cancelled(),
        }
    }
}

impl Drop for SessionNotificationPump {
    fn drop(&mut self) {
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

/// Spawn a background task that drains `notif_rx` and emits each item as
/// `SpurEventBody::AgentNotification` tagged with `spur_session_id`. The
/// returned `JoinHandle` MUST be aborted when the owning session is
/// retired — otherwise the task keeps emitting events with the stale
/// session id against a reused connection.
///
/// `Lagged(n)` is emitted as a host-visible `BrainError` and the loop
/// continues; `Closed` terminates.
pub fn spawn_session_notification_pump(
    notif_rx: Receiver<SessionNotification>,
    spur_session_id: SessionId,
    funnel: FunnelHandle,
) -> JoinHandle<()> {
    spawn_session_notification_pump_task(notif_rx, spur_session_id, funnel, None)
}

pub(crate) fn spawn_observed_session_notification_pump(
    notif_rx: Receiver<SessionNotification>,
    spur_session_id: SessionId,
    funnel: FunnelHandle,
) -> SessionNotificationPump {
    let (barrier_tx, barrier_rx) = mpsc::unbounded_channel();
    let task =
        spawn_session_notification_pump_task(notif_rx, spur_session_id, funnel, Some(barrier_rx));
    SessionNotificationPump {
        task: Some(task),
        barrier_tx,
    }
}

fn spawn_session_notification_pump_task(
    mut notif_rx: Receiver<SessionNotification>,
    spur_session_id: SessionId,
    funnel: FunnelHandle,
    mut barrier_rx: Option<mpsc::UnboundedReceiver<BarrierReply>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut snapshot = NotificationPumpSnapshot::default();
        loop {
            tokio::select! {
                outcome = notif_rx.recv() => match outcome {
                    Ok(notification) => {
                        emit_notification(
                            &funnel,
                            &spur_session_id,
                            notification,
                            &mut snapshot,
                        );
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        emit_lag(&funnel, &spur_session_id, skipped);
                    }
                    Err(RecvError::Closed) => break,
                },
                command = receive_barrier(&mut barrier_rx), if barrier_rx.is_some() => {
                    let Some(reply) = command else {
                        barrier_rx = None;
                        continue;
                    };
                    let closed = drain_ready_notifications(
                        &mut notif_rx,
                        &funnel,
                        &spur_session_id,
                        &mut snapshot,
                    );
                    let _ = reply.send(snapshot);
                    if closed {
                        break;
                    }
                }
            }
        }
    })
}

async fn receive_barrier(
    barrier_rx: &mut Option<mpsc::UnboundedReceiver<BarrierReply>>,
) -> Option<BarrierReply> {
    match barrier_rx.as_mut() {
        Some(receiver) => receiver.recv().await,
        None => futures::future::pending().await,
    }
}

fn drain_ready_notifications(
    notif_rx: &mut Receiver<SessionNotification>,
    funnel: &FunnelHandle,
    spur_session_id: &SessionId,
    snapshot: &mut NotificationPumpSnapshot,
) -> bool {
    loop {
        match notif_rx.try_recv() {
            Ok(notification) => {
                emit_notification(funnel, spur_session_id, notification, snapshot);
            }
            Err(TryRecvError::Lagged(skipped)) => {
                emit_lag(funnel, spur_session_id, skipped);
            }
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Closed) => return true,
        }
    }
}

fn emit_notification(
    funnel: &FunnelHandle,
    spur_session_id: &SessionId,
    notification: SessionNotification,
    snapshot: &mut NotificationPumpSnapshot,
) {
    funnel.emit(SpurEventBody::AgentNotification {
        session: spur_session_id.clone(),
        notification: Box::new(notification),
    });
    snapshot.emitted = snapshot.emitted.saturating_add(1);
}

fn emit_lag(funnel: &FunnelHandle, spur_session_id: &SessionId, skipped: u64) {
    tracing::warn!(
        skipped,
        session = %spur_session_id,
        "session notification pump lagged"
    );
    funnel.emit(SpurEventBody::BrainError {
        session: spur_session_id.clone(),
        message: format!(
            "Session notification stream lost {skipped} message(s); output may be incomplete."
        ),
    });
}

pub fn spawn_agent_client_request_pump(
    mut request_rx: tokio::sync::mpsc::UnboundedReceiver<AgentClientRequestPayload>,
    spur_session_id: SessionId,
    funnel: FunnelHandle,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(payload) = request_rx.recv().await {
            let message = match payload.kind {
                AgentClientRequestKind::Logout => {
                    "Agent requested logout; authentication is required before continuing."
                        .to_string()
                }
                AgentClientRequestKind::Authenticate { method_id } => {
                    format!(
                        "Agent requested authentication with method '{method_id}', but Spur credential forwarding is not configured."
                    )
                }
            };
            funnel.emit(SpurEventBody::AuthRequired {
                session: spur_session_id.clone(),
                message,
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct TaskDropProbe(Arc<AtomicBool>);

    impl Drop for TaskDropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retire_until_forced_aborts_and_joins_taken_task() {
        let task_dropped = Arc::new(AtomicBool::new(false));
        let task_started = Arc::new(Notify::new());
        let task = tokio::spawn({
            let task_dropped = Arc::clone(&task_dropped);
            let task_started = Arc::clone(&task_started);
            async move {
                let _probe = TaskDropProbe(task_dropped);
                task_started.notify_one();
                std::future::pending::<()>().await;
            }
        });
        task_started.notified().await;
        let (barrier_tx, _barrier_rx) = mpsc::unbounded_channel();
        let pump = SessionNotificationPump {
            task: Some(task),
            barrier_tx,
        };
        let force = CancellationToken::new();
        force.cancel();

        let forced = tokio::time::timeout(Duration::from_secs(1), pump.retire_until_forced(&force))
            .await
            .expect("forced pump retirement must acknowledge promptly");

        assert!(forced);
        assert!(
            task_dropped.load(Ordering::SeqCst),
            "pump retirement returned before its taken task acknowledged abort"
        );
    }

    #[tokio::test]
    async fn agent_client_request_pump_emits_auth_required_events() {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (funnel, mut event_rx) = crate::event_funnel::test_channel();
        let session = SessionId("brain-session".to_string());
        let handle = spawn_agent_client_request_pump(request_rx, session.clone(), funnel);

        request_tx
            .send(AgentClientRequestPayload {
                kind: AgentClientRequestKind::Logout,
            })
            .expect("pump should still be running");
        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("logout should emit an event")
            .expect("event channel should remain open");
        assert!(matches!(
            event,
            SpurEventBody::AuthRequired {
                session: ref actual_session,
                ..
            } if actual_session == &session
        ));

        request_tx
            .send(AgentClientRequestPayload {
                kind: AgentClientRequestKind::Authenticate {
                    method_id: "api-key".to_string(),
                },
            })
            .expect("pump should still be running");
        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("authenticate should emit an event")
            .expect("event channel should remain open");
        assert!(matches!(
            event,
            SpurEventBody::AuthRequired {
                session: ref actual_session,
                message
            } if actual_session == &session && message.contains("api-key")
        ));

        handle.abort();
    }
}
