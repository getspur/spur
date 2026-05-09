pub(in crate::orchestrator) async fn candidate_set_for_target(
    bundle: &crate::peer_mailbox::PeerMailboxBundle,
    delegation_id: &spur_acp::domain::delegation::DelegationId,
) -> Vec<crate::peer_mailbox::LedgerEntry> {
    let mut candidates = bundle.ledger.pending_for_target(delegation_id).await;
    candidates.extend(
        bundle
            .ledger
            .non_terminal_entries()
            .await
            .into_iter()
            .filter(|entry| &entry.envelope.target_delegation_id == delegation_id),
    );
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|entry| seen.insert(entry.envelope.message_id));
    candidates
}

/// Forced-terminal-timeout drain. Waits up to `quiet_window` for peer-ack
/// notifications scoped to `delegation_id`. Each ack resets the window.
/// The drain is also bounded by `max_total`. After either deadline elapses,
/// delivered non-terminal peer messages are forced to `Ignored` with a reason
/// that classifies the exit path.
pub(in crate::orchestrator) async fn drain_peer_acks_with_timeout(
    bundle: &crate::peer_mailbox::PeerMailboxBundle,
    delegation_id: &spur_acp::domain::delegation::DelegationId,
    quiet_window: std::time::Duration,
    max_total: std::time::Duration,
    brain_session_id: &spur_acp::BrainSessionId,
    funnel: &crate::event_funnel::FunnelHandle,
    mut ack_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    use spur_acp::domain::peer_message::{LedgerState, TerminalOutcome};

    let cap_deadline = tokio::time::Instant::now() + max_total;
    let drain_start = tokio::time::Instant::now();
    let mut cap_hit = false;
    let mut acks_received: u32 = 0;
    let candidates_at_start = candidate_set_for_target(bundle, delegation_id).await.len() as u32;

    funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDrainStarted {
        brain_session_id: brain_session_id.to_string(),
        target_delegation_id: delegation_id.clone(),
        candidates_at_start,
        cap_ms: max_total.as_millis() as u64,
        quiet_window_ms: quiet_window.as_millis() as u64,
    });

    loop {
        let now = tokio::time::Instant::now();
        if now >= cap_deadline {
            cap_hit = true;
            break;
        }

        let quiet_deadline = now + quiet_window;
        let next_deadline = quiet_deadline.min(cap_deadline);
        let waiting_for_cap = next_deadline == cap_deadline;

        match tokio::time::timeout_at(next_deadline, ack_rx.recv()).await {
            Ok(Some(())) => {
                acks_received = acks_received.saturating_add(1);
            }
            Ok(None) => break,
            Err(_) => {
                cap_hit = waiting_for_cap;
                break;
            }
        }
    }

    let actual_elapsed_ms = drain_start.elapsed().as_millis() as u64;

    let candidates = candidate_set_for_target(bundle, delegation_id).await;
    let remaining_messages = candidates
        .iter()
        .filter(|entry| {
            matches!(
                entry.state,
                LedgerState::Delivered | LedgerState::DeliveredInflight
            )
        })
        .count() as u32;

    if cap_hit {
        funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDrainCappedOut {
            brain_session_id: brain_session_id.to_string(),
            target_delegation_id: delegation_id.clone(),
            acks_received,
            remaining_messages,
            cap_ms: max_total.as_millis() as u64,
            actual_elapsed_ms,
        });
    } else if remaining_messages > 0 {
        funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDrainTimedOut {
            brain_session_id: brain_session_id.to_string(),
            target_delegation_id: delegation_id.clone(),
            acks_received,
            remaining_messages,
            cap_ms: max_total.as_millis() as u64,
            quiet_window_ms: quiet_window.as_millis() as u64,
            actual_elapsed_ms,
        });
    }

    let reason = if cap_hit {
        "drain_capped"
    } else {
        "drain_timeout"
    };
    for entry in candidates {
        let message_id = entry.envelope.message_id;
        if !matches!(
            entry.state,
            LedgerState::Delivered | LedgerState::DeliveredInflight
        ) {
            continue;
        }
        if let Err(err) = bundle
            .router
            .record_terminal(
                brain_session_id.as_session_id().0.as_str(),
                &message_id,
                TerminalOutcome::Ignored {
                    reason: reason.into(),
                },
            )
            .await
        {
            tracing::warn!(
                message_id = ?message_id,
                ?err,
                "peer mailbox: forced-terminal-timeout drain failed"
            );
        }
    }
}
