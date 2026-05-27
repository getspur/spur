use super::*;

impl App {
    pub(super) fn process_review(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::SubmitReview {
                executor_id,
                attempt_n,
                decision,
            } => {
                let has_review = self
                    .lineage
                    .node(&spur_core::ExecutorId(executor_id.clone()))
                    .map(|n| n.pending_review.is_some())
                    .unwrap_or(false);
                if !has_review {
                    tracing::warn!(executor_id = %executor_id, "SubmitReview ignored: no pending review on this node");
                    return None;
                }
                let decision_label = format!("{decision:?}");
                let label = format!("{decision_label}…");
                let pending_dispatch = Action::SubmitReviewDispatch {
                    executor_id: executor_id.clone(),
                    attempt_n,
                    decision,
                };
                let now = Instant::now();
                let displaced = self.tombstones.install_and_get_displaced(Tombstone {
                    view: ViewId::Dashboard,
                    kind: TombstoneKind::QueuedRemote {
                        pending: pending_dispatch,
                    },
                    label: label.clone(),
                    created_at: now,
                    expires_at: now + Duration::from_secs(3),
                });
                if let Some(displaced_ts) = displaced {
                    if let TombstoneKind::QueuedRemote { pending } = displaced_ts.kind {
                        self.process_action(pending);
                    }
                }
                self.flash_hint(
                    format!("{label} — press u to revert (3s)"),
                    Duration::from_secs(2),
                );
                self.dirty = true;
                None
            }

            Action::SubmitReviewDispatch {
                executor_id,
                attempt_n,
                decision,
            } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::SubmitReview {
                        executor_id: executor_id.clone(),
                        attempt_n,
                        decision: decision.clone(),
                    });
                }
                // Optimistically reflect the resolution locally so the UI
                // updates immediately without waiting for the authoritative
                // event to round-trip.
                self.lineage.apply(&spur_acp::SpurEvent::now(
                    spur_acp::SpurEventBody::ExecutorReviewResolved {
                        id: executor_id,
                        decision: to_wire_decision(&decision),
                    },
                ));
                self.flash_hint_short("Sent.");
                self.dirty = true;
                #[cfg(feature = "analytics")]
                self.sync_live_cost_active_sessions();
                None
            }

            _ => None,
        }
    }
}
