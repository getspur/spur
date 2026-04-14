//! Fan-out pump: drains a connection-scoped broadcast of SessionNotifications
//! and emits SpurEventBody::AgentNotification onto the SpurEvent bus via the
//! funnel, tagged with the caller-supplied spur_session_id.

use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::task::JoinHandle;

use agent_client_protocol::SessionNotification;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::types::SessionId;

use crate::event_funnel::FunnelHandle;

/// Spawn a background task that drains `notif_rx` and emits each item as
/// `SpurEventBody::AgentNotification` tagged with `spur_session_id`. The
/// returned `JoinHandle` MUST be aborted when the owning session is
/// retired — otherwise the task keeps emitting events with the stale
/// session id against a reused connection.
///
/// `Lagged(n)` is logged and the loop continues; `Closed` terminates.
pub fn spawn_session_notification_pump(
    mut notif_rx: Receiver<SessionNotification>,
    spur_session_id: SessionId,
    funnel: FunnelHandle,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match notif_rx.recv().await {
                Ok(notif) => {
                    funnel.emit(SpurEventBody::AgentNotification {
                        session: spur_session_id.clone(),
                        notification: Box::new(notif),
                    });
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(
                        skipped = n,
                        session = %spur_session_id,
                        "session notification pump lagged"
                    );
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}
