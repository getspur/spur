//! Brain turn scheduler for continuation delivery guarantees v3.1.
//!
//! Runtime-free sync: no spawned tasks, no timer ownership, and no runtime
//! requirement to construct or exercise the scheduler in unit tests.

use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::{BrainContinuation, DeferReason, DelegationKey, DropReason};
use spur_acp::types::{BrainSessionId, SessionId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::continuation_bridge::{ContinuationEventSink, OverflowBuf, MERGE_BUDGET_DEFAULT_BYTES};
use crate::orchestrator::InteractiveInput;

pub const CANCEL_GRACE_DEFAULT: Duration = Duration::from_millis(750);
pub const DRAIN_CAP: usize = 32;
pub const MAX_REQUEUE_ATTEMPTS: u32 = 8;
pub const REQUEUE_CHANNEL_CAPACITY: usize = DRAIN_CAP * 4;

#[derive(Debug)]
pub enum ScheduledAction {
    /// Fire a user turn. Caller flattens into `PromptRequest`.
    UserPrompt(InteractiveInput),
    /// Fire a merged turn: user input foreground, continuations as background
    /// blocks. Delivery is committed only after prompt dispatch succeeds.
    MergedPrompt {
        user: InteractiveInput,
        batch: DrainedBatch,
    },
    /// Fire an autonomous continuation turn.
    ContinuationPrompt(DrainedBatch),
    /// No work to dispatch right now. `deadline` is the next scheduler wakeup
    /// if liveness requires one.
    IdleUntil { deadline: Option<Instant> },
}

#[derive(Clone)]
pub(crate) struct PendingContinuation {
    continuation: BrainContinuation,
    requeue_count: u32,
}

impl PendingContinuation {
    fn new(continuation: BrainContinuation) -> Self {
        Self {
            continuation,
            requeue_count: 0,
        }
    }

    fn key(&self) -> DelegationKey {
        DelegationKey::from(&self.continuation)
    }
}

/// Owns the split-lane queues and scheduling policy for brain turns.
pub struct BrainScheduler {
    /// FIFO queue of user-originated inputs (from the TUI). Always drains
    /// before continuations (human priority).
    pending_user: VecDeque<InteractiveInput>,
    /// FIFO queue of detached worker outcomes awaiting a safe scheduling
    /// window.
    pending_continuations: VecDeque<PendingContinuation>,
    /// Already-delivered continuation keys for dedup.
    delivered_ids: HashSet<DelegationKey>,
    /// Active brain session guard for session-swap eviction.
    ///
    /// This is the brain's SPUR-side identity (`BrainSessionId` wraps
    /// `spur_session_id`), NOT the ACP protocol session id. See
    /// [`Self::note_session_swap`] for the invariant and the rationale.
    active_session: Option<BrainSessionId>,
    /// Shared flag so `TurnGuard` and scheduler methods can coexist without
    /// borrowing the scheduler for the guard's lifetime.
    turn_in_flight: Arc<AtomicBool>,
    /// Post-cancel grace suppresses autonomous continuations until this instant.
    cancel_grace_until: Option<Instant>,
    /// Grace window duration.
    cancel_grace_window: Duration,
    /// Internal requeue channel. Receive end drained at the top of `next()`.
    requeue_rx: mpsc::Receiver<RequeueCommand>,
    requeue_tx: mpsc::Sender<RequeueCommand>,
    /// Typed event sink for terminal drops and retriable deferrals.
    event_sink: Arc<dyn ContinuationEventSink>,
}

impl BrainScheduler {
    pub fn new(
        active_session: Option<BrainSessionId>,
        event_sink: Arc<dyn ContinuationEventSink>,
    ) -> Self {
        Self::with_requeue_capacity(active_session, event_sink, REQUEUE_CHANNEL_CAPACITY)
    }

    fn with_requeue_capacity(
        active_session: Option<BrainSessionId>,
        event_sink: Arc<dyn ContinuationEventSink>,
        requeue_capacity: usize,
    ) -> Self {
        let cancel_grace_window = std::env::var("SPUR_CANCEL_GRACE_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(CANCEL_GRACE_DEFAULT);
        let (requeue_tx, requeue_rx) = mpsc::channel(requeue_capacity);
        Self {
            pending_user: VecDeque::new(),
            pending_continuations: VecDeque::new(),
            delivered_ids: HashSet::new(),
            active_session,
            turn_in_flight: Arc::new(AtomicBool::new(false)),
            cancel_grace_until: None,
            cancel_grace_window,
            requeue_rx,
            requeue_tx,
            event_sink,
        }
    }

    pub fn turn_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.turn_in_flight)
    }

    pub fn push_user(&mut self, input: InteractiveInput) {
        self.pending_user.push_back(input);
    }

    /// Ingress. Enforces session matching and dedup semantics.
    pub fn push_continuation(&mut self, c: BrainContinuation) {
        let key = DelegationKey::from(&c);

        if self
            .active_session
            .as_ref()
            .map(BrainSessionId::as_session_id)
            != Some(&c.brain_session)
        {
            tracing::debug!(
                continuation_probe = true,
                site = "B_push_continuation",
                delegation_id = %c.delegation_id,
                attempt = c.attempt,
                outcome = "stale_session",
                "scheduler: drop continuation with stale session"
            );
            self.emit_drop(&c, DropReason::StaleSession);
            return;
        }

        if self.delivered_ids.contains(&key) {
            tracing::debug!(
                continuation_probe = true,
                site = "B_push_continuation",
                delegation_id = %c.delegation_id,
                attempt = c.attempt,
                outcome = "already_delivered",
                "scheduler: drop continuation already delivered"
            );
            self.emit_drop(&c, DropReason::AlreadyDelivered);
            return;
        }

        if self
            .pending_continuations
            .iter()
            .any(|queued| queued.key() == key)
        {
            tracing::debug!(
                continuation_probe = true,
                site = "B_push_continuation",
                delegation_id = %c.delegation_id,
                attempt = c.attempt,
                outcome = "dedup_pending",
                "scheduler: drop continuation already pending"
            );
            return;
        }

        let delegation_id = c.delegation_id.clone();
        let attempt = c.attempt;
        self.pending_continuations
            .push_back(PendingContinuation::new(c));
        tracing::debug!(
            continuation_probe = true,
            site = "B_push_continuation",
            delegation_id = %delegation_id,
            attempt,
            outcome = "enqueued",
            pending_depth = self.pending_continuations.len(),
            user_depth = self.pending_user.len(),
            turn_in_flight = self.turn_in_flight.load(Ordering::SeqCst),
            "scheduler: continuation enqueued"
        );
    }

    pub fn note_turn_started(&mut self) {
        self.turn_in_flight.store(true, Ordering::SeqCst);
    }

    pub fn note_turn_finished(&mut self) {
        self.turn_in_flight.store(false, Ordering::SeqCst);
    }

    /// Call after cancel has fully resolved (stream drained / force-timeout
    /// fired).
    pub fn note_cancel_resolved(&mut self, now: Instant) {
        self.cancel_grace_until = Some(now + self.cancel_grace_window);
    }

    /// Called on successful prompt dispatch.
    pub fn commit_partial(
        &mut self,
        batch: DrainedBatch,
        delivered_keys: Vec<DelegationKey>,
        dropped_terminal: Vec<(DelegationKey, DropReason)>,
        spilled_with_reason: Option<Vec<(DelegationKey, DeferReason)>>,
    ) {
        let items = batch.into_items();
        let batch_keys: HashSet<_> = items.iter().map(PendingContinuation::key).collect();
        let session_hint: SessionId = self
            .active_session
            .as_ref()
            .map(|id| id.as_session_id().clone())
            .or_else(|| {
                items
                    .first()
                    .map(|item| item.continuation.brain_session.clone())
            })
            .unwrap_or_default();

        let mut valid_delivered = HashSet::new();
        let mut valid_dropped = HashMap::new();
        let mut valid_spilled = HashMap::new();
        let mut mismatched_batch_keys = HashSet::new();
        let mut spilled = Vec::new();
        let explicit_spills = spilled_with_reason.is_some();

        let mut delivered_seen = HashSet::new();
        for key in delivered_keys {
            if !delivered_seen.insert(key.clone()) {
                debug_assert!(
                    false,
                    "commit_partial delivered_keys contains duplicate key: {:?}",
                    key
                );
                continue;
            }
            if !batch_keys.contains(&key) {
                debug_assert!(
                    false,
                    "commit_partial delivered_keys contains unknown key: {:?}",
                    key
                );
                self.emit_unknown_mismatched_drop(&key, &session_hint);
                continue;
            }
            valid_delivered.insert(key);
        }

        let mut dropped_seen = HashSet::new();
        for (key, reason) in dropped_terminal {
            if !dropped_seen.insert(key.clone()) {
                debug_assert!(
                    false,
                    "commit_partial dropped_terminal contains duplicate key: {:?}",
                    key
                );
                continue;
            }
            if !batch_keys.contains(&key) {
                debug_assert!(
                    false,
                    "commit_partial dropped_terminal contains unknown key: {:?}",
                    key
                );
                self.emit_unknown_mismatched_drop(&key, &session_hint);
                continue;
            }
            valid_dropped.insert(key, reason);
        }

        if let Some(spilled_with_reason) = spilled_with_reason {
            let mut spilled_seen = HashSet::new();
            for (key, reason) in spilled_with_reason {
                if !spilled_seen.insert(key.clone()) {
                    debug_assert!(
                        false,
                        "commit_partial spilled_with_reason contains duplicate key: {:?}",
                        key
                    );
                    continue;
                }
                if !batch_keys.contains(&key) {
                    debug_assert!(
                        false,
                        "commit_partial spilled_with_reason contains unknown key: {:?}",
                        key
                    );
                    self.emit_unknown_mismatched_drop(&key, &session_hint);
                    continue;
                }
                valid_spilled.insert(key, reason);
            }
        }

        let overlap: HashSet<_> = valid_delivered
            .iter()
            .filter(|key| valid_dropped.contains_key(*key) || valid_spilled.contains_key(*key))
            .cloned()
            .chain(
                valid_dropped
                    .keys()
                    .filter(|key| valid_spilled.contains_key(*key))
                    .cloned(),
            )
            .collect();
        for key in overlap {
            debug_assert!(
                false,
                "commit_partial key present in multiple partitions: {:?}",
                key
            );
            valid_delivered.remove(&key);
            valid_dropped.remove(&key);
            valid_spilled.remove(&key);
            mismatched_batch_keys.insert(key);
        }

        for item in items {
            let key = item.key();
            if mismatched_batch_keys.contains(&key) {
                self.emit_drop(&item.continuation, DropReason::MismatchedCommitKeys);
                continue;
            }
            if let Some(reason) = valid_dropped.remove(&key) {
                self.emit_drop(&item.continuation, reason);
                continue;
            }
            if valid_delivered.remove(&key) {
                self.delivered_ids.insert(key);
                continue;
            }
            if let Some(reason) = valid_spilled.remove(&key) {
                self.requeue_immediately(item, reason);
                continue;
            }
            if explicit_spills {
                debug_assert!(
                    false,
                    "commit_partial spilled_with_reason omitted batch key: {:?}",
                    key
                );
            }
            spilled.push(item);
        }

        if !spilled.is_empty() {
            self.apply_requeue_command(RequeueCommand::Spilled { items: spilled });
        }
    }

    pub fn commit(&mut self, batch: DrainedBatch) {
        let delivered_keys = batch
            .items()
            .iter()
            .map(DelegationKey::from)
            .collect::<Vec<_>>();
        self.commit_partial(batch, delivered_keys, Vec::new(), None);
    }

    /// Called on prompt-dispatch failure.
    pub fn rollback(
        &mut self,
        batch: DrainedBatch,
        dropped_terminal: Vec<(DelegationKey, DropReason)>,
    ) {
        let items = batch.into_items();
        let batch_keys: HashSet<_> = items.iter().map(PendingContinuation::key).collect();
        let session_hint: SessionId = self
            .active_session
            .as_ref()
            .map(|id| id.as_session_id().clone())
            .or_else(|| {
                items
                    .first()
                    .map(|item| item.continuation.brain_session.clone())
            })
            .unwrap_or_default();
        let mut valid_dropped = HashMap::new();
        let mut dropped_seen = HashSet::new();

        for (key, reason) in dropped_terminal {
            if !dropped_seen.insert(key.clone()) {
                debug_assert!(
                    false,
                    "rollback dropped_terminal contains duplicate key: {:?}",
                    key
                );
                continue;
            }
            if !batch_keys.contains(&key) {
                debug_assert!(
                    false,
                    "rollback dropped_terminal contains unknown key: {:?}",
                    key
                );
                self.emit_unknown_mismatched_drop(&key, &session_hint);
                continue;
            }
            valid_dropped.insert(key, reason);
        }

        for item in items {
            let key = item.key();
            if let Some(reason) = valid_dropped.remove(&key) {
                self.emit_drop(&item.continuation, reason);
                continue;
            }
            self.requeue_immediately(item, DeferReason::PromptDispatchFailure);
        }
    }

    /// Called on brain-session retirement.
    ///
    /// # Invariant
    ///
    /// `new_active` MUST be the brain's **SPUR** session id
    /// (`BrainSession::spur_session_id`), NOT the ACP protocol session id
    /// (`BrainSession::acp_session_id`). These are distinct UUIDs:
    /// `spur_session_id` is generated by SPUR; `acp_session_id` is
    /// returned by the ACP agent's `new_session()` response. The MCP
    /// server stamps every `BrainContinuation.brain_session` with
    /// `spur_session_id` via its `brain_session_id` field, so this
    /// scheduler MUST key on the same id for continuation delivery to
    /// match.
    ///
    /// The `BrainSessionId` newtype is used to make this a compile-time
    /// invariant — a prior bug (pre-fix commit 5c50e24) passed
    /// `SessionId(acp_session_id)` here, silently dropping every detached
    /// continuation as `StaleSession`. Callers must now wrap
    /// `spur_session_id` as `BrainSessionId` (via `.into()`), so there is
    /// no `SessionId` constructor path that can produce the wrong domain.
    ///
    /// See `crates/spur-core/tests/continuation_brain_session_wiring.rs`.
    pub fn note_session_swap(
        &mut self,
        new_active: Option<BrainSessionId>,
        overflow: &OverflowBuf,
    ) {
        while let Some(item) = self.pending_continuations.pop_front() {
            self.emit_drop(&item.continuation, DropReason::SessionSwap);
        }

        let mut overflow_guard = lock_overflow(overflow);
        while let Some((_session, continuation)) = overflow_guard.pop_front() {
            self.emit_drop(&continuation, DropReason::SessionSwap);
        }
        drop(overflow_guard);

        self.delivered_ids.clear();
        self.active_session = new_active;
    }

    /// Operational metric. Returns the depth of the internal requeue channel.
    pub fn requeue_depth(&self) -> usize {
        self.requeue_rx.len()
    }

    /// Pure sync: given the current clock, return the next action.
    pub fn next(&mut self, now: Instant) -> ScheduledAction {
        self.drain_requeue_channel();
        self.clear_grace_if_user_arrived();

        if self.turn_in_flight.load(Ordering::SeqCst) {
            return ScheduledAction::IdleUntil { deadline: None };
        }

        if let Some(user) = self.pending_user.pop_front() {
            if self.pending_continuations.is_empty() {
                return ScheduledAction::UserPrompt(user);
            }
            return ScheduledAction::MergedPrompt {
                user,
                batch: self.drain_continuations_for_delivery(),
            };
        }

        if self.pending_continuations.is_empty() {
            return ScheduledAction::IdleUntil { deadline: None };
        }

        if self.in_cancel_grace(now) {
            return ScheduledAction::IdleUntil {
                deadline: self.cancel_grace_until,
            };
        }

        ScheduledAction::ContinuationPrompt(self.drain_continuations_for_delivery())
    }

    fn clear_grace_if_user_arrived(&mut self) {
        if !self.pending_user.is_empty() {
            self.cancel_grace_until = None;
        }
    }

    fn in_cancel_grace(&self, now: Instant) -> bool {
        match self.cancel_grace_until {
            Some(deadline) => now < deadline,
            None => false,
        }
    }

    fn emit_drop(&self, continuation: &BrainContinuation, reason: DropReason) {
        self.event_sink.emit(SpurEventBody::ContinuationDropped {
            delegation_id: continuation.delegation_id.clone(),
            attempt: continuation.attempt,
            brain_session: continuation.brain_session.clone(),
            reason,
        });
    }

    fn emit_unknown_mismatched_drop(&self, key: &DelegationKey, session_hint: &SessionId) {
        self.event_sink.emit(SpurEventBody::ContinuationDropped {
            delegation_id: key.delegation_id.clone(),
            attempt: key.attempt,
            brain_session: session_hint.clone(),
            reason: DropReason::MismatchedCommitKeys,
        });
    }

    fn emit_defer(
        &self,
        continuation: &BrainContinuation,
        requeue_count: u32,
        reason: DeferReason,
    ) {
        self.event_sink.emit(SpurEventBody::ContinuationDeferred {
            delegation_id: continuation.delegation_id.clone(),
            attempt: continuation.attempt,
            brain_session: continuation.brain_session.clone(),
            requeue_count,
            reason,
        });
    }

    fn push_internal(&mut self, item: PendingContinuation) {
        self.pending_continuations.push_back(item);
    }

    fn drain_requeue_channel(&mut self) {
        while let Ok(command) = self.requeue_rx.try_recv() {
            self.apply_requeue_command(command);
        }
    }

    fn apply_requeue_command(&mut self, command: RequeueCommand) {
        match command {
            RequeueCommand::Spilled { items } => {
                for item in items {
                    let reason = spill_reason(&item.continuation);
                    self.requeue_immediately(item, reason);
                }
            }
            RequeueCommand::Leaked { items } => {
                for item in items {
                    self.requeue_immediately(item, DeferReason::LeakedBatch);
                }
            }
        }
    }

    fn requeue_immediately(&mut self, mut item: PendingContinuation, reason: DeferReason) {
        item.requeue_count += 1;
        if item.requeue_count > MAX_REQUEUE_ATTEMPTS {
            self.emit_drop(&item.continuation, DropReason::MaxRequeueExceeded);
            return;
        }

        self.emit_defer(&item.continuation, item.requeue_count, reason);
        self.push_internal(item);
    }

    /// Drains at most `DRAIN_CAP` continuations for delivery.
    fn drain_continuations_for_delivery(&mut self) -> DrainedBatch {
        let count = self.pending_continuations.len().min(DRAIN_CAP);
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(item) = self.pending_continuations.pop_front() {
                items.push(item);
            }
        }
        DrainedBatch::new(
            items,
            self.requeue_tx.clone(),
            Arc::downgrade(&self.event_sink),
        )
    }

    #[cfg(test)]
    pub(crate) fn pending_user_len(&self) -> usize {
        self.pending_user.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_continuation_len(&self) -> usize {
        self.pending_continuations.len()
    }
}

fn spill_reason(continuation: &BrainContinuation) -> DeferReason {
    DeferReason::BudgetSpill {
        budget_bytes: MERGE_BUDGET_DEFAULT_BYTES,
        continuation_bytes: serde_json::to_vec(continuation)
            .map(|json| json.len())
            .unwrap_or_default(),
    }
}

fn lock_overflow(
    overflow: &OverflowBuf,
) -> tokio::sync::MutexGuard<'_, VecDeque<(SessionId, BrainContinuation)>> {
    match overflow.try_lock() {
        Ok(guard) => guard,
        Err(_) => futures::executor::block_on(overflow.lock()),
    }
}

#[must_use = "DrainedBatch must be passed to commit / commit_partial / rollback; dropping unhandled requeues the items with a Deferred(LeakedBatch) event"]
pub struct DrainedBatch {
    items: Vec<PendingContinuation>,
    view: Vec<BrainContinuation>,
    requeue_tx: mpsc::Sender<RequeueCommand>,
    event_sink_weak: Weak<dyn ContinuationEventSink>,
    consumed: bool,
}

impl DrainedBatch {
    fn new(
        items: Vec<PendingContinuation>,
        requeue_tx: mpsc::Sender<RequeueCommand>,
        event_sink_weak: Weak<dyn ContinuationEventSink>,
    ) -> Self {
        let view = items
            .iter()
            .map(|item| item.continuation.clone())
            .collect::<Vec<_>>();
        Self {
            items,
            view,
            requeue_tx,
            event_sink_weak,
            consumed: false,
        }
    }

    pub fn items(&self) -> &[BrainContinuation] {
        &self.view
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn into_items(mut self) -> Vec<PendingContinuation> {
        self.consumed = true;
        std::mem::take(&mut self.items)
    }
}

impl fmt::Debug for DrainedBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrainedBatch")
            .field("len", &self.items.len())
            .field("consumed", &self.consumed)
            .finish()
    }
}

impl Drop for DrainedBatch {
    fn drop(&mut self) {
        if self.consumed || self.items.is_empty() {
            return;
        }

        let items = std::mem::take(&mut self.items);
        self.view.clear();

        match self.requeue_tx.try_send(RequeueCommand::Leaked { items }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(RequeueCommand::Leaked { items }))
            | Err(mpsc::error::TrySendError::Closed(RequeueCommand::Leaked { items })) => {
                if let Some(sink) = self.event_sink_weak.upgrade() {
                    for item in items {
                        sink.emit(SpurEventBody::ContinuationDropped {
                            delegation_id: item.continuation.delegation_id.clone(),
                            attempt: item.continuation.attempt,
                            brain_session: item.continuation.brain_session.clone(),
                            reason: DropReason::RequeueChannelFull,
                        });
                    }
                }
            }
            Err(_) => unreachable!("DrainedBatch::Drop only emits RequeueCommand::Leaked"),
        }
    }
}

pub(crate) enum RequeueCommand {
    Spilled { items: Vec<PendingContinuation> },
    Leaked { items: Vec<PendingContinuation> },
}

#[must_use = "TurnGuard must be bound to a variable; an unbound guard drops immediately and returns turn_in_flight to false"]
pub struct TurnGuard {
    flag: Arc<AtomicBool>,
}

impl TurnGuard {
    pub fn arm(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self { flag }
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::domain::{ContinuationPayload, ContinuationSource};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Mutex;

    use crate::continuation_bridge::new_overflow_buf;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<SpurEventBody>>,
    }

    impl RecordingSink {
        fn snapshot(&self) -> Vec<SpurEventBody> {
            self.events.lock().unwrap().clone()
        }
    }

    impl ContinuationEventSink for RecordingSink {
        fn emit(&self, body: SpurEventBody) {
            self.events.lock().unwrap().push(body);
        }
    }

    fn mk_scheduler(active_session: Option<SessionId>) -> (BrainScheduler, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let scheduler = BrainScheduler::new(active_session.map(BrainSessionId::from), sink.clone());
        (scheduler, sink)
    }

    fn mk_scheduler_with_capacity(
        active_session: Option<BrainSessionId>,
        requeue_capacity: usize,
    ) -> (BrainScheduler, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let scheduler =
            BrainScheduler::with_requeue_capacity(active_session, sink.clone(), requeue_capacity);
        (scheduler, sink)
    }

    fn mk_cont(id: &str, attempt: u32, brain_session: &SessionId) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            attempt,
            brain_session: brain_session.clone(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None,
                diff_summary: None,
                worker_branch: None,
                artifact_ref: None,
                estimated_cost_micros: None,
                artifact_id: None,
                fetch_hint: None,
                base_hint: None,
                setup_conflict_topology: None,
            },
            created_at_wall: Utc::now(),
            created_at_mono: Instant::now(),
        }
    }

    fn message_input() -> InteractiveInput {
        InteractiveInput::Message {
            blocks: vec![],
            interrupt: false,
        }
    }

    fn only_drop_event(events: &[SpurEventBody]) -> (&str, u32, SessionId, DropReason) {
        assert_eq!(events.len(), 1);
        match &events[0] {
            SpurEventBody::ContinuationDropped {
                delegation_id,
                attempt,
                brain_session,
                reason,
            } => (
                delegation_id.as_str(),
                *attempt,
                brain_session.clone(),
                reason.clone(),
            ),
            other => panic!("expected ContinuationDropped, got {:?}", other),
        }
    }

    fn only_defer_event(events: &[SpurEventBody]) -> (&str, u32, SessionId, u32, DeferReason) {
        assert_eq!(events.len(), 1);
        match &events[0] {
            SpurEventBody::ContinuationDeferred {
                delegation_id,
                attempt,
                brain_session,
                requeue_count,
                reason,
            } => (
                delegation_id.as_str(),
                *attempt,
                brain_session.clone(),
                *requeue_count,
                reason.clone(),
            ),
            other => panic!("expected ContinuationDeferred, got {:?}", other),
        }
    }

    fn continuation_batch(scheduler: &mut BrainScheduler) -> DrainedBatch {
        match scheduler.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(batch) => batch,
            other => panic!("expected ContinuationPrompt, got {:?}", other),
        }
    }

    #[test]
    fn new_scheduler_is_empty() {
        let session = SessionId::new();
        let (scheduler, _sink) = mk_scheduler(Some(session));
        assert_eq!(scheduler.pending_user_len(), 0);
        assert_eq!(scheduler.pending_continuation_len(), 0);
        assert_eq!(scheduler.requeue_depth(), 0);
    }

    #[test]
    fn test_push_continuation_session_match_accepts() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));

        scheduler.push_continuation(mk_cont("id-1", 1, &session));

        assert_eq!(scheduler.pending_continuation_len(), 1);
        assert!(sink.snapshot().is_empty());
    }

    #[test]
    fn test_push_continuation_session_mismatch_emits_stale_session() {
        let active = SessionId::new();
        let stale = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(active));

        scheduler.push_continuation(mk_cont("id-1", 1, &stale));

        assert_eq!(scheduler.pending_continuation_len(), 0);
        let events = sink.snapshot();
        let (delegation_id, attempt, brain_session, reason) = only_drop_event(&events);
        assert_eq!(delegation_id, "id-1");
        assert_eq!(attempt, 1);
        assert_eq!(brain_session, stale);
        assert_eq!(reason, DropReason::StaleSession);
    }

    #[test]
    fn test_push_continuation_already_delivered_emits() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let continuation = mk_cont("id-1", 1, &session);
        let key = DelegationKey::from(&continuation);

        scheduler.push_continuation(continuation.clone());
        let batch = continuation_batch(&mut scheduler);
        scheduler.commit(batch);

        assert!(scheduler.delivered_ids.contains(&key));

        scheduler.push_continuation(continuation);

        let events = sink.snapshot();
        let (delegation_id, attempt, brain_session, reason) = only_drop_event(&events);
        assert_eq!(delegation_id, "id-1");
        assert_eq!(attempt, 1);
        assert_eq!(brain_session, session);
        assert_eq!(reason, DropReason::AlreadyDelivered);
    }

    #[test]
    fn test_push_continuation_pending_dup_silent() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));

        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));

        assert_eq!(scheduler.pending_continuation_len(), 1);
        assert!(sink.snapshot().is_empty());
    }

    #[test]
    fn next_is_idle_when_everything_empty() {
        let session = SessionId::new();
        let (mut scheduler, _sink) = mk_scheduler(Some(session));
        assert!(matches!(
            scheduler.next(Instant::now()),
            ScheduledAction::IdleUntil { deadline: None }
        ));
    }

    #[test]
    fn next_returns_user_prompt_when_user_queued_and_idle() {
        let session = SessionId::new();
        let (mut scheduler, _sink) = mk_scheduler(Some(session));
        scheduler.push_user(message_input());

        assert!(matches!(
            scheduler.next(Instant::now()),
            ScheduledAction::UserPrompt(_)
        ));
    }

    #[test]
    fn next_returns_idle_while_turn_in_flight_even_if_user_queued() {
        let session = SessionId::new();
        let (mut scheduler, _sink) = mk_scheduler(Some(session));
        scheduler.push_user(message_input());
        scheduler.note_turn_started();

        assert!(matches!(
            scheduler.next(Instant::now()),
            ScheduledAction::IdleUntil { deadline: None }
        ));
    }

    #[test]
    fn next_fires_continuation_only_when_idle_and_no_user_pending() {
        let session = SessionId::new();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));

        match scheduler.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(batch) => assert_eq!(batch.len(), 1),
            other => panic!("expected ContinuationPrompt, got {:?}", other),
        }
    }

    #[test]
    fn next_user_beats_continuation_when_both_queued() {
        let session = SessionId::new();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        scheduler.push_user(message_input());

        match scheduler.next(Instant::now()) {
            ScheduledAction::MergedPrompt { batch, .. } => {
                assert_eq!(batch.len(), 1);
            }
            other => panic!("expected MergedPrompt, got {:?}", other),
        }
    }

    #[test]
    fn next_coalesces_multiple_continuations_fifo() {
        let session = SessionId::new();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        scheduler.push_continuation(mk_cont("id-2", 1, &session));
        scheduler.push_continuation(mk_cont("id-3", 1, &session));

        match scheduler.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(batch) => {
                assert_eq!(batch.len(), 3);
                assert_eq!(batch.items()[0].delegation_id, "id-1");
                assert_eq!(batch.items()[2].delegation_id, "id-3");
            }
            other => panic!("expected ContinuationPrompt, got {:?}", other),
        }
    }

    #[test]
    fn next_suppresses_continuation_during_cancel_grace() {
        let session = SessionId::new();
        let now = Instant::now();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        scheduler.note_cancel_resolved(now);

        assert!(matches!(
            scheduler.next(now + Duration::from_millis(100)),
            ScheduledAction::IdleUntil { deadline: Some(_) }
        ));
        assert!(matches!(
            scheduler.next(now + Duration::from_millis(2000)),
            ScheduledAction::ContinuationPrompt(_)
        ));
    }

    #[test]
    fn grace_cleared_when_user_arrives_during_in_flight_turn() {
        let session = SessionId::new();
        let now = Instant::now();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));

        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        scheduler.note_turn_started();
        scheduler.note_cancel_resolved(now);
        scheduler.push_user(message_input());

        assert!(matches!(
            scheduler.next(now + Duration::from_millis(100)),
            ScheduledAction::IdleUntil { deadline: None }
        ));

        scheduler.note_turn_finished();

        match scheduler.next(now + Duration::from_millis(200)) {
            ScheduledAction::MergedPrompt { batch, .. } => {
                assert_eq!(batch.len(), 1);
            }
            other => panic!("expected MergedPrompt, got {:?}", other),
        }
    }

    #[test]
    fn test_commit_moves_to_delivered_ids() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let continuation = mk_cont("id-1", 1, &session);
        let key = DelegationKey::from(&continuation);

        scheduler.push_continuation(continuation.clone());
        let batch = continuation_batch(&mut scheduler);
        scheduler.commit(batch);

        assert!(scheduler.delivered_ids.contains(&key));
        scheduler.push_continuation(continuation);
        let events = sink.snapshot();
        let (_, _, _, reason) = only_drop_event(&events);
        assert_eq!(reason, DropReason::AlreadyDelivered);
    }

    #[test]
    fn test_commit_partial_three_way_partition() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let a = mk_cont("id-a", 1, &session);
        let b = mk_cont("id-b", 1, &session);
        let c = mk_cont("id-c", 1, &session);
        let a_key = DelegationKey::from(&a);
        let b_key = DelegationKey::from(&b);
        let spill_reason = DeferReason::BudgetSpill {
            budget_bytes: 1234,
            continuation_bytes: 567,
        };

        scheduler.push_continuation(a.clone());
        scheduler.push_continuation(b.clone());
        scheduler.push_continuation(c.clone());
        let batch = continuation_batch(&mut scheduler);
        scheduler.commit_partial(
            batch,
            vec![a_key.clone()],
            vec![(
                DelegationKey::from(&c),
                DropReason::OversizedSingleItem {
                    continuation_bytes: 9001,
                    budget_bytes: MERGE_BUDGET_DEFAULT_BYTES,
                },
            )],
            Some(vec![(b_key.clone(), spill_reason.clone())]),
        );

        assert!(scheduler.delivered_ids.contains(&a_key));
        assert_eq!(scheduler.pending_continuation_len(), 1);
        assert_eq!(scheduler.pending_continuations[0].key(), b_key);

        let events = sink.snapshot();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| matches!(
            event,
            SpurEventBody::ContinuationDeferred {
                delegation_id,
                reason,
                ..
            } if delegation_id == "id-b" && reason == &spill_reason
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            SpurEventBody::ContinuationDropped {
                delegation_id,
                reason: DropReason::OversizedSingleItem { .. },
                ..
            } if delegation_id == "id-c"
        )));
    }

    #[test]
    fn test_commit_partial_delivered_and_terminal_disjoint() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let continuation = mk_cont("id-1", 1, &session);
        let key = DelegationKey::from(&continuation);

        scheduler.push_continuation(continuation);
        let batch = continuation_batch(&mut scheduler);

        #[cfg(debug_assertions)]
        {
            let _ = &sink;
            let result = catch_unwind(AssertUnwindSafe(|| {
                scheduler.commit_partial(
                    batch,
                    vec![key.clone()],
                    vec![(key, DropReason::SessionSwap)],
                    None,
                )
            }));
            assert!(result.is_err());
        }

        #[cfg(not(debug_assertions))]
        {
            scheduler.commit_partial(
                batch,
                vec![key.clone()],
                vec![(key, DropReason::SessionSwap)],
                None,
            );
            let events = sink.snapshot();
            let (_, _, _, reason) = only_drop_event(&events);
            assert_eq!(reason, DropReason::MismatchedCommitKeys);
        }
    }

    #[test]
    fn test_v3_1_dropped_terminal_duplicate_key() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let continuation = mk_cont("id-1", 1, &session);
        let key = DelegationKey::from(&continuation);

        scheduler.push_continuation(continuation);
        let batch = continuation_batch(&mut scheduler);

        #[cfg(debug_assertions)]
        {
            let _ = &sink;
            let result = catch_unwind(AssertUnwindSafe(|| {
                scheduler.commit_partial(
                    batch,
                    vec![],
                    vec![
                        (key.clone(), DropReason::SessionSwap),
                        (key, DropReason::StaleSession),
                    ],
                    None,
                )
            }));
            assert!(result.is_err());
        }

        #[cfg(not(debug_assertions))]
        {
            scheduler.commit_partial(
                batch,
                vec![],
                vec![
                    (key.clone(), DropReason::SessionSwap),
                    (key, DropReason::StaleSession),
                ],
                None,
            );
            let events = sink.snapshot();
            let (_, _, _, reason) = only_drop_event(&events);
            assert_eq!(reason, DropReason::SessionSwap);
        }
    }

    #[test]
    fn test_v3_1_delivered_keys_duplicate() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let continuation = mk_cont("id-1", 1, &session);
        let key = DelegationKey::from(&continuation);

        scheduler.push_continuation(continuation);
        let batch = continuation_batch(&mut scheduler);

        #[cfg(debug_assertions)]
        {
            let _ = &sink;
            let result = catch_unwind(AssertUnwindSafe(|| {
                scheduler.commit_partial(batch, vec![key.clone(), key], vec![], None)
            }));
            assert!(result.is_err());
        }

        #[cfg(not(debug_assertions))]
        {
            scheduler.commit_partial(batch, vec![key.clone(), key.clone()], vec![], None);
            assert!(scheduler.delivered_ids.contains(&key));
            assert_eq!(scheduler.delivered_ids.len(), 1);
        }
    }

    #[test]
    fn test_rollback_requeues_all_with_defer_event() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        scheduler.push_continuation(mk_cont("id-2", 1, &session));

        let batch = continuation_batch(&mut scheduler);
        scheduler.rollback(batch, vec![]);

        assert_eq!(scheduler.pending_continuation_len(), 2);
        let events = sink.snapshot();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| matches!(
            event,
            SpurEventBody::ContinuationDeferred {
                requeue_count: 1,
                reason: DeferReason::PromptDispatchFailure,
                ..
            }
        )));
    }

    #[test]
    fn test_rollback_drops_terminal_and_requeues_remainder() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let drop_me = mk_cont("drop-me", 1, &session);
        let keep_me = mk_cont("keep-me", 1, &session);
        let keep_key = DelegationKey::from(&keep_me);

        scheduler.push_continuation(drop_me.clone());
        scheduler.push_continuation(keep_me);

        let batch = continuation_batch(&mut scheduler);
        scheduler.rollback(
            batch,
            vec![(
                DelegationKey::from(&drop_me),
                DropReason::OversizedSingleItem {
                    continuation_bytes: 9_999,
                    budget_bytes: MERGE_BUDGET_DEFAULT_BYTES,
                },
            )],
        );

        assert_eq!(scheduler.pending_continuation_len(), 1);
        assert_eq!(scheduler.pending_continuations[0].key(), keep_key);

        let events = sink.snapshot();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| matches!(
            event,
            SpurEventBody::ContinuationDropped {
                delegation_id,
                reason: DropReason::OversizedSingleItem { .. },
                ..
            } if delegation_id == "drop-me"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            SpurEventBody::ContinuationDeferred {
                delegation_id,
                reason: DeferReason::PromptDispatchFailure,
                requeue_count: 1,
                ..
            } if delegation_id == "keep-me"
        )));
    }

    #[test]
    fn test_drained_batch_leaked_requeues_on_drop() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));

        let batch = continuation_batch(&mut scheduler);
        drop(batch);

        assert_eq!(scheduler.requeue_depth(), 1);

        match scheduler.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(batch) => {
                assert_eq!(batch.len(), 1);
                assert_eq!(batch.items()[0].delegation_id, "id-1");
            }
            other => panic!(
                "expected ContinuationPrompt after leak recovery, got {:?}",
                other
            ),
        }

        let events = sink.snapshot();
        let (_, _, _, requeue_count, reason) = only_defer_event(&events);
        assert_eq!(requeue_count, 1);
        assert_eq!(reason, DeferReason::LeakedBatch);
    }

    #[test]
    fn test_drained_batch_leaked_channel_full_emits_dropped() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler_with_capacity(Some(session.clone().into()), 1);

        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        let first = scheduler.drain_continuations_for_delivery();
        drop(first);
        assert_eq!(scheduler.requeue_depth(), 1);

        scheduler.push_continuation(mk_cont("id-2", 1, &session));
        let second = scheduler.drain_continuations_for_delivery();
        drop(second);

        let events = sink.snapshot();
        let (_, _, _, reason) = only_drop_event(&events);
        assert_eq!(reason, DropReason::RequeueChannelFull);
        assert_eq!(scheduler.requeue_depth(), 1);
    }

    #[test]
    fn test_turn_guard_clears_flag_on_drop() {
        let session = SessionId::new();
        let (scheduler, _sink) = mk_scheduler(Some(session));
        let flag = scheduler.turn_flag();

        {
            let _guard = TurnGuard::arm(flag.clone());
            assert!(flag.load(Ordering::SeqCst));
        }

        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_turn_guard_scheduler_callable_while_armed() {
        let session = SessionId::new();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        let continuation = mk_cont("id-1", 1, &session);
        let key = DelegationKey::from(&continuation);
        scheduler.push_continuation(continuation);

        let batch = continuation_batch(&mut scheduler);
        let _guard = TurnGuard::arm(scheduler.turn_flag());
        scheduler.commit(batch);

        assert!(scheduler.delivered_ids.contains(&key));
    }

    #[test]
    fn test_note_session_swap_drains_pending_and_overflow() {
        let old_session = SessionId::new();
        let new_session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(old_session.clone()));
        let overflow = new_overflow_buf();

        scheduler.push_continuation(mk_cont("pending-1", 1, &old_session));
        scheduler.push_continuation(mk_cont("pending-2", 1, &old_session));

        {
            let mut guard = overflow.try_lock().unwrap();
            guard.push_back((old_session.clone(), mk_cont("overflow-1", 1, &old_session)));
            guard.push_back((old_session.clone(), mk_cont("overflow-2", 1, &old_session)));
        }

        scheduler.note_session_swap(Some(new_session.clone().into()), &overflow);

        assert_eq!(scheduler.pending_continuation_len(), 0);
        assert!(overflow.try_lock().unwrap().is_empty());
        assert_eq!(
            scheduler.active_session,
            Some(BrainSessionId::from(new_session))
        );

        let events = sink.snapshot();
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|event| matches!(
            event,
            SpurEventBody::ContinuationDropped {
                reason: DropReason::SessionSwap,
                ..
            }
        )));
    }

    #[test]
    fn test_note_session_swap_clears_delivered_ids() {
        let old_session = SessionId::new();
        let new_session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(old_session.clone()));
        let overflow = new_overflow_buf();
        let old_cont = mk_cont("id-1", 1, &old_session);
        let key = DelegationKey::from(&old_cont);

        scheduler.push_continuation(old_cont);
        let batch = continuation_batch(&mut scheduler);
        scheduler.commit(batch);
        assert!(scheduler.delivered_ids.contains(&key));

        scheduler.note_session_swap(Some(new_session.clone().into()), &overflow);
        assert!(scheduler.delivered_ids.is_empty());

        scheduler.push_continuation(mk_cont("id-1", 1, &new_session));
        assert_eq!(scheduler.pending_continuation_len(), 1);
        assert!(sink.snapshot().is_empty());
    }

    #[test]
    fn test_idle_until_grace_deadline_returned() {
        let session = SessionId::new();
        let now = Instant::now();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));
        scheduler.note_cancel_resolved(now);

        match scheduler.next(now + Duration::from_millis(1)) {
            ScheduledAction::IdleUntil {
                deadline: Some(deadline),
            } => assert_eq!(deadline, now + CANCEL_GRACE_DEFAULT),
            other => panic!("expected IdleUntil with deadline, got {:?}", other),
        }
    }

    #[test]
    fn test_requeue_depth_bounded() {
        let session = SessionId::new();
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("id-1", 1, &session));

        for _ in 0..MAX_REQUEUE_ATTEMPTS {
            let batch = continuation_batch(&mut scheduler);
            scheduler.rollback(batch, vec![]);
        }

        let final_batch = continuation_batch(&mut scheduler);
        scheduler.rollback(final_batch, vec![]);

        assert_eq!(scheduler.pending_continuation_len(), 0);
        let events = sink.snapshot();
        assert!(matches!(
            events.last(),
            Some(SpurEventBody::ContinuationDropped {
                reason: DropReason::MaxRequeueExceeded,
                ..
            })
        ));
    }
}
