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

use spur_acp::connection::{AgentClientRequestKind, AgentClientRequestPayload};
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
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;

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
