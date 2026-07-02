//! Integration tests for the broadcast → SpurEvent notification pump.
//!
//! These tests verify `spawn_session_notification_pump` in isolation:
//!
//! 1. `pump_emits_agent_notification_with_correct_session_id` — publishes a
//!    `SessionNotification` via a broadcast sender and asserts the funnel
//!    emits `SpurEventBody::AgentNotification` tagged with the pump's
//!    `spur_session_id`.
//!
//! 2. `pump_abort_stops_emission_for_retired_session` — regression for
//!    commit 408dc23: aborts pump 1, starts pump 2 on the same broadcast
//!    with a different session id, and asserts only pump 2 emits.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::types::SessionId;
use spur_acp::{AvailableCommand, AvailableCommandsUpdate, SessionNotification, SessionUpdate};
use spur_core::event_funnel::spawn_funnel;
use spur_core::notification_pump::spawn_session_notification_pump;
use tokio::sync::broadcast;

/// Drain a limited number of `SpurEvent`s from `rx` within `timeout`,
/// returning all `AgentNotification` bodies received.
async fn collect_agent_notifications(
    rx: &mut broadcast::Receiver<SpurEvent>,
    timeout: Duration,
) -> Vec<(SessionId, Box<spur_acp::SessionNotification>)> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut results = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                if let SpurEventBody::AgentNotification {
                    session,
                    notification,
                } = ev.body
                {
                    results.push((session, notification));
                }
            }
            // Timeout or channel closed → stop collecting.
            Ok(Err(_)) | Err(_) => break,
        }
    }

    results
}

#[tokio::test(flavor = "multi_thread")]
async fn pump_emits_agent_notification_with_correct_session_id() {
    // ── Setup funnel + event broadcast ──────────────────────────────────────
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(256);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx.clone(), seq);

    // ── Create notification broadcast ────────────────────────────────────────
    let (notif_tx, notif_rx) = broadcast::channel::<SessionNotification>(16);

    // ── Spawn pump with spur_session_id = "sess-A" ──────────────────────────
    let spur_session_id = SessionId("sess-A".to_string());
    let _pump_handle = spawn_session_notification_pump(notif_rx, spur_session_id.clone(), funnel);

    // ── Exercise: send one notification ─────────────────────────────────────
    let sent_update = SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
        AvailableCommand::new("cmd1", "desc1"),
    ]));
    let acp_session_id = spur_acp::AcpSessionId::new("acp-sess-1");
    notif_tx
        .send(SessionNotification::new(acp_session_id, sent_update))
        .expect("notification broadcast send should succeed");

    // ── Assert: within 2 s, observe the AgentNotification on the event bus ──
    let notifications = collect_agent_notifications(&mut bcast_rx, Duration::from_secs(2)).await;

    assert_eq!(
        notifications.len(),
        1,
        "expected exactly one AgentNotification on the event bus, got {}",
        notifications.len()
    );

    let (emitted_session, emitted_notif) = &notifications[0];
    assert_eq!(
        emitted_session, &spur_session_id,
        "AgentNotification must be tagged with the pump's spur_session_id"
    );

    match &emitted_notif.update {
        SessionUpdate::AvailableCommandsUpdate(update) => {
            assert_eq!(update.available_commands.len(), 1);
            assert_eq!(update.available_commands[0].name, "cmd1");
        }
        other => panic!("expected AvailableCommandsUpdate, got {other:?}"),
    }

    // ── Teardown: dropping notif_tx causes pump to receive Closed and exit ──
    drop(notif_tx);
}

#[tokio::test(flavor = "multi_thread")]
async fn pump_abort_stops_emission_for_retired_session() {
    // ── Setup funnel + event broadcast ──────────────────────────────────────
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(256);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx.clone(), seq);

    // ── Create notification broadcast ────────────────────────────────────────
    let (notif_tx, notif_rx_1) = broadcast::channel::<SessionNotification>(16);

    // ── Spawn pump 1 with "sess-A", then immediately abort it ────────────────
    let pump_handle_1 = spawn_session_notification_pump(
        notif_rx_1,
        SessionId("sess-A".to_string()),
        funnel.clone(),
    );
    pump_handle_1.abort();
    // Give the runtime a moment to process the abort.
    tokio::task::yield_now().await;

    // ── Spawn pump 2 with "sess-B" on the same broadcast ────────────────────
    let notif_rx_2 = notif_tx.subscribe();
    let _pump_handle_2 =
        spawn_session_notification_pump(notif_rx_2, SessionId("sess-B".to_string()), funnel);

    // ── Exercise: send a notification ────────────────────────────────────────
    let acp_session_id = spur_acp::AcpSessionId::new("acp-sess-2");
    notif_tx
        .send(SessionNotification::new(
            acp_session_id,
            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
                AvailableCommand::new("cmd2", "desc2"),
            ])),
        ))
        .expect("notification broadcast send should succeed");

    // ── Assert: exactly one notification arrives, tagged with "sess-B" ───────
    // Allow 500 ms to catch any spurious doubles from an unaborted pump 1.
    let notifications =
        collect_agent_notifications(&mut bcast_rx, Duration::from_millis(500)).await;

    assert_eq!(
        notifications.len(),
        1,
        "expected exactly one AgentNotification (from pump 2 only), got {}: {:?}",
        notifications.len(),
        notifications
            .iter()
            .map(|(s, _)| s.0.as_str())
            .collect::<Vec<_>>()
    );

    let (emitted_session, _) = &notifications[0];
    assert_eq!(
        emitted_session.0.as_str(),
        "sess-B",
        "notification must come from pump 2 (sess-B), not the aborted pump 1"
    );

    drop(notif_tx);
}
