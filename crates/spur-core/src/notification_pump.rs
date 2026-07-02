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

use tokio::sync::broadcast::{error::RecvError, Receiver};
use tokio::task::JoinHandle;

use spur_acp::domain::events::SpurEventBody;
use spur_acp::types::SessionId;
use spur_acp::SessionNotification;

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
