//! Helper for draining ACP session notifications from a `prompt()` call.
//!
//! `NativeAcpConnection::prompt()` returns an empty stream and instead routes
//! all `SessionNotification`s through a connection-scoped broadcast channel.
//! Other transports (stdio, cli_wrap, stream_json) still use the per-call
//! stream.  `drive_prompt_notifications` handles both cases:
//!
//! - It subscribes to the broadcast **before** calling `prompt()` so that no
//!   notification emitted during the async prompt setup is missed.
//! - It then runs a `tokio::select!` loop that polls the compat stream (empty
//!   for native) AND the broadcast concurrently.
//! - Once the compat stream closes it starts a 100 ms grace window to flush
//!   any in-flight broadcast messages before returning.

use std::time::Duration;

use futures::StreamExt;
use spur_acp::{PromptRequest, SessionNotification};
use tokio::sync::broadcast::error::RecvError;

use spur_acp::connection::AgentConnection;

/// Outcome of a single broadcast receive attempt.
enum BcastOutcome {
    Notification(Box<SessionNotification>),
    Lagged,
    Closed,
}

/// Call `connection.prompt(prompt_request)`, drain all resulting
/// `SessionNotification`s (from either the compat stream or the
/// connection-scoped broadcast), and invoke `on_notification` for each one.
///
/// Returns `Ok(())` when the prompt turn is complete (stream closed + 100 ms
/// grace window drained), or `Err(…)` if `prompt()` itself returns an error.
///
/// # Broadcast handling
///
/// For `NativeAcpConnection` the compat stream is empty; all notifications
/// arrive on the broadcast.  For all other transports `subscribe_session_notifications`
/// returns `None` and only the compat stream is drained.
pub(crate) async fn drive_prompt_notifications<F>(
    connection: &mut dyn AgentConnection,
    prompt_request: PromptRequest,
    mut on_notification: F,
) -> anyhow::Result<()>
where
    F: FnMut(SessionNotification),
{
    // Subscribe before calling prompt() so we don't miss notifications
    // emitted during the async setup inside prompt().
    let mut notif_rx = connection.subscribe_session_notifications();

    let prompt_fut = connection.prompt(prompt_request);
    tokio::pin!(prompt_fut);

    let mut prompt_stream: Option<
        std::pin::Pin<Box<dyn futures::Stream<Item = SessionNotification> + Send>>,
    > = None;
    let mut grace_deadline: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            biased;

            // 1. Resolve the prompt() future until it produces a stream.
            result = &mut prompt_fut, if prompt_stream.is_none() && grace_deadline.is_none() => {
                match result {
                    Ok(stream) => {
                        prompt_stream = Some(stream);
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }

            // 2. Drain the compat stream (empty for native, real for others).
            maybe_notif = async {
                match prompt_stream.as_mut() {
                    Some(s) => s.next().await,
                    None => futures::future::pending().await,
                }
            }, if prompt_stream.is_some() => {
                match maybe_notif {
                    Some(notif) => on_notification(notif),
                    None => {
                        // Stream closed → prompt turn finished. Start grace window
                        // for any LocalSet-scheduled stragglers on the broadcast.
                        prompt_stream = None;
                        grace_deadline = Some(
                            tokio::time::Instant::now() + Duration::from_millis(100),
                        );
                    }
                }
            }

            // 3. Drain the broadcast (real path for native transports).
            bcast_outcome = poll_bcast(&mut notif_rx), if notif_rx.is_some() => {
                match bcast_outcome {
                    BcastOutcome::Notification(notif) => on_notification(*notif),
                    BcastOutcome::Lagged => {
                        // Already warned; continue.
                    }
                    BcastOutcome::Closed => {
                        // Broadcast closed → connection tearing down.
                        notif_rx = None;
                    }
                }
            }

            // 4. Grace window expires → exit loop.
            _ = async {
                match grace_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => futures::future::pending().await,
                }
            }, if grace_deadline.is_some() => {
                break;
            }
        }
    }

    Ok(())
}

/// Poll a single item from an optional broadcast receiver.
///
/// Panics (unreachable) if called with `rx = None` — callers must guard with
/// the `if notif_rx.is_some()` precondition on the select arm.
async fn poll_bcast(
    rx: &mut Option<tokio::sync::broadcast::Receiver<SessionNotification>>,
) -> BcastOutcome {
    match rx.as_mut() {
        Some(r) => match r.recv().await {
            Ok(notif) => BcastOutcome::Notification(Box::new(notif)),
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "drive_prompt_notifications: broadcast lagged");
                BcastOutcome::Lagged
            }
            Err(RecvError::Closed) => BcastOutcome::Closed,
        },
        None => {
            // This branch is unreachable because the select arm is guarded
            // by `if notif_rx.is_some()`. Use pending() to satisfy the
            // type checker defensively.
            futures::future::pending().await
        }
    }
}
