use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::{ContentBlock, TextContent};
use chrono::Utc;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::{
    BrainContinuation, ContinuationPayload, ContinuationSource, DelegationKey, DropReason,
};
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::{
    render_autonomous_turn_with_spill_v2, render_merged_turn_with_spill_v2, ContinuationEventSink,
};
use spur_core::scheduler::{BrainScheduler, ScheduledAction};
use spur_core::InteractiveInput;

const TEST_BUDGET_BYTES: usize = 2_048;
const USER_BLOCK_BYTES: usize = 768;

#[derive(Debug, Clone, Copy)]
enum SizeClass {
    Small,
    Medium,
    Large,
    Oversized,
}

impl SizeClass {
    const fn index(self) -> usize {
        match self {
            Self::Small => 0,
            Self::Medium => 1,
            Self::Large => 2,
            Self::Oversized => 3,
        }
    }
}

#[derive(Debug, Clone)]
struct ArrivalSpec {
    delegation_id: String,
    attempt: u32,
    size_class: SizeClass,
}

#[derive(Debug, Clone)]
struct OutcomeStep {
    arrivals_to_push: u8,
    queue_user: bool,
    advance_ms: u16,
    dispatch_success: bool,
}

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

fn size_class_strategy() -> impl Strategy<Value = SizeClass> {
    prop_oneof![
        Just(SizeClass::Small),
        Just(SizeClass::Medium),
        Just(SizeClass::Large),
        Just(SizeClass::Oversized),
    ]
}

fn arb_arrival_sequence() -> impl Strategy<Value = Vec<ArrivalSpec>> {
    prop::collection::vec((1u32..=3, size_class_strategy()), 1..=12).prop_map(|raw| {
        raw.into_iter()
            .enumerate()
            .map(|(idx, (attempt, size_class))| ArrivalSpec {
                delegation_id: format!("delegation-{idx:03}"),
                attempt,
                size_class,
            })
            .collect()
    })
}

fn arb_outcome_sequence() -> impl Strategy<Value = Vec<OutcomeStep>> {
    prop::collection::vec((0u8..=3, any::<bool>(), 0u16..=5, any::<bool>()), 1..=24).prop_map(
        |raw| {
            raw.into_iter()
                .map(
                    |(arrivals_to_push, queue_user, advance_ms, dispatch_success)| OutcomeStep {
                        arrivals_to_push,
                        queue_user,
                        advance_ms,
                        dispatch_success,
                    },
                )
                .collect()
        },
    )
}

fn arb_oversized_continuation() -> impl Strategy<Value = ArrivalSpec> {
    (1u32..=3).prop_map(|attempt| ArrivalSpec {
        delegation_id: "oversized-continuation".to_string(),
        attempt,
        size_class: SizeClass::Oversized,
    })
}

fn mk_cont(spec: &ArrivalSpec, brain_session: &SessionId) -> BrainContinuation {
    BrainContinuation {
        delegation_id: spec.delegation_id.clone().into(),
        attempt: spec.attempt,
        brain_session: brain_session.clone(),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("x".repeat(summary_len_for_class(spec.size_class))),
            diff_summary: None,
            worker_branch: None,
            artifact_ref: None,
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
        },
        created_at_wall: Utc::now(),
        created_at_mono: Instant::now(),
    }
}

fn summary_len_for_class(size_class: SizeClass) -> usize {
    size_profiles()[size_class.index()]
}

fn size_profiles() -> &'static [usize; 4] {
    static SIZE_PROFILES: OnceLock<[usize; 4]> = OnceLock::new();
    SIZE_PROFILES.get_or_init(compute_size_profiles)
}

fn compute_size_profiles() -> [usize; 4] {
    let mut profiles = [None; 4];

    for len in 0..=16_384 {
        let class = classify_probe_summary_len(len);
        let slot = &mut profiles[class.index()];
        if slot.is_none() {
            *slot = Some(len);
        }
        if profiles.iter().all(Option::is_some) {
            break;
        }
    }

    profiles.map(|len| len.expect("all size classes should have a representative summary length"))
}

fn classify_probe_summary_len(summary_len: usize) -> SizeClass {
    let probe = BrainContinuation {
        delegation_id: "probe-000".into(),
        attempt: 1,
        brain_session: SessionId("brain-proptest".into()),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("x".repeat(summary_len)),
            diff_summary: None,
            worker_branch: None,
            artifact_ref: None,
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
        },
        created_at_wall: Utc::now(),
        created_at_mono: Instant::now(),
    };

    if fits_budget(&probe, TEST_BUDGET_BYTES / 4) {
        SizeClass::Small
    } else if fits_budget(&probe, TEST_BUDGET_BYTES / 2) {
        SizeClass::Medium
    } else if fits_budget(&probe, TEST_BUDGET_BYTES) {
        SizeClass::Large
    } else {
        SizeClass::Oversized
    }
}

fn fits_budget(continuation: &BrainContinuation, budget_bytes: usize) -> bool {
    !render_autonomous_turn_with_spill_v2(std::slice::from_ref(continuation), budget_bytes)
        .delivered_keys
        .is_empty()
}

fn user_input() -> InteractiveInput {
    InteractiveInput::Message {
        blocks: vec![ContentBlock::Text(TextContent::new(
            "u".repeat(USER_BLOCK_BYTES),
        ))],
        interrupt: false,
    }
}

fn record_delivered(
    delivered_counts: &mut HashMap<DelegationKey, usize>,
    delivered_keys: &[DelegationKey],
) {
    for key in delivered_keys {
        *delivered_counts.entry(key.clone()).or_default() += 1;
    }
}

fn drive_one_action(
    scheduler: &mut BrainScheduler,
    now: Instant,
    dispatch_success: bool,
    delivered_counts: &mut HashMap<DelegationKey, usize>,
) -> bool {
    match scheduler.next(now) {
        ScheduledAction::UserPrompt(_) | ScheduledAction::IdleUntil { .. } => false,
        ScheduledAction::MergedPrompt { user, batch } => {
            let user_blocks = match user {
                InteractiveInput::Message { blocks, .. } => blocks,
                other => panic!("unexpected non-message user input in property test: {other:?}"),
            };
            let outcome =
                render_merged_turn_with_spill_v2(&user_blocks, batch.items(), TEST_BUDGET_BYTES);
            commit_or_rollback(
                scheduler,
                batch,
                outcome,
                dispatch_success,
                delivered_counts,
            );
            true
        }
        ScheduledAction::ContinuationPrompt(batch) => {
            let outcome = render_autonomous_turn_with_spill_v2(batch.items(), TEST_BUDGET_BYTES);
            let must_commit = outcome.blocks.is_empty();
            commit_or_rollback(
                scheduler,
                batch,
                outcome,
                dispatch_success || must_commit,
                delivered_counts,
            );
            true
        }
    }
}

fn drain_to_quiescence(
    scheduler: &mut BrainScheduler,
    now: Instant,
    delivered_counts: &mut HashMap<DelegationKey, usize>,
) {
    loop {
        if !drive_one_action(scheduler, now, true, delivered_counts) {
            break;
        }
    }
}

fn commit_or_rollback(
    scheduler: &mut BrainScheduler,
    batch: spur_core::scheduler::DrainedBatch,
    outcome: spur_core::continuation_bridge::RenderOutcome,
    dispatch_success: bool,
    delivered_counts: &mut HashMap<DelegationKey, usize>,
) {
    let dropped_terminal = outcome
        .dropped_oversized
        .iter()
        .map(|(key, continuation_bytes)| {
            (
                key.clone(),
                DropReason::OversizedSingleItem {
                    continuation_bytes: *continuation_bytes,
                    budget_bytes: TEST_BUDGET_BYTES,
                },
            )
        })
        .collect();
    if !dispatch_success {
        scheduler.rollback(batch, dropped_terminal);
        return;
    }

    record_delivered(delivered_counts, &outcome.delivered_keys);
    let spilled_with_reason = Some(
        outcome
            .deferred_spill
            .iter()
            .map(|(continuation, reason)| (DelegationKey::from(continuation), reason.clone()))
            .collect(),
    );
    scheduler.commit_partial(
        batch,
        outcome.delivered_keys,
        dropped_terminal,
        spilled_with_reason,
    );
}

fn drop_counts(events: &[SpurEventBody]) -> HashMap<DelegationKey, usize> {
    let mut counts = HashMap::new();
    for event in events {
        if let SpurEventBody::ContinuationDropped {
            delegation_id,
            attempt,
            ..
        } = event
        {
            *counts
                .entry(DelegationKey {
                    delegation_id: delegation_id.clone(),
                    attempt: *attempt,
                })
                .or_default() += 1;
        }
    }
    counts
}

fn defer_count_for_key(events: &[SpurEventBody], key: &DelegationKey) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SpurEventBody::ContinuationDeferred {
                    delegation_id,
                    attempt,
                    ..
                } if delegation_id == &key.delegation_id && attempt == &key.attempt
            )
        })
        .count()
}

fn oversized_drop_count_for_key(events: &[SpurEventBody], key: &DelegationKey) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SpurEventBody::ContinuationDropped {
                    delegation_id,
                    attempt,
                    reason: DropReason::OversizedSingleItem { .. },
                    ..
                } if delegation_id == &key.delegation_id && attempt == &key.attempt
            )
        })
        .count()
}

fn run_scenario(
    arrivals: &[ArrivalSpec],
    steps: &[OutcomeStep],
) -> (
    Vec<SpurEventBody>,
    HashSet<DelegationKey>,
    HashMap<DelegationKey, usize>,
) {
    let sink = Arc::new(RecordingSink::default());
    let session = SessionId("brain-proptest".into());
    let mut scheduler = BrainScheduler::new(Some(session.clone().into()), sink.clone());
    let mut delivered_counts = HashMap::new();
    let mut pushed_keys = HashSet::new();
    let mut next_arrival = 0usize;
    let mut now = Instant::now();

    for step in steps {
        for _ in 0..step.arrivals_to_push {
            if next_arrival >= arrivals.len() {
                break;
            }
            let continuation = mk_cont(&arrivals[next_arrival], &session);
            pushed_keys.insert(DelegationKey::from(&continuation));
            scheduler.push_continuation(continuation);
            next_arrival += 1;
        }

        if step.queue_user {
            scheduler.push_user(user_input());
        }

        now += Duration::from_millis(u64::from(step.advance_ms));
        let _ = drive_one_action(
            &mut scheduler,
            now,
            step.dispatch_success,
            &mut delivered_counts,
        );
    }

    while next_arrival < arrivals.len() {
        let continuation = mk_cont(&arrivals[next_arrival], &session);
        pushed_keys.insert(DelegationKey::from(&continuation));
        scheduler.push_continuation(continuation);
        next_arrival += 1;
    }

    drain_to_quiescence(&mut scheduler, now, &mut delivered_counts);
    (sink.snapshot(), pushed_keys, delivered_counts)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// INV-D1 property. Every pushed continuation must eventually
    /// terminate exactly once as either Delivered or Dropped(reason).
    #[test]
    fn inv_d1_every_continuation_terminates_exactly_once(
        arrivals in arb_arrival_sequence(),
        outcomes in arb_outcome_sequence(),
    ) {
        let (events, pushed_keys, delivered_counts) = run_scenario(&arrivals, &outcomes);
        let dropped_counts = drop_counts(&events);

        for key in pushed_keys {
            let delivered = delivered_counts.get(&key).copied().unwrap_or(0);
            let dropped = dropped_counts.get(&key).copied().unwrap_or(0);
            prop_assert_eq!(
                delivered + dropped,
                1,
                "continuation {:?} ended with delivered={} dropped={} events={:?}",
                key,
                delivered,
                dropped,
                events,
            );
        }
    }

    /// INV-D6 property. A continuation whose standalone cost exceeds the
    /// delivery budget must never enter the requeue loop.
    #[test]
    fn inv_d6_oversized_never_requeues(
        oversized in arb_oversized_continuation(),
        other_arrivals in arb_arrival_sequence(),
    ) {
        let sink = Arc::new(RecordingSink::default());
        let session = SessionId("brain-proptest".into());
        let mut scheduler = BrainScheduler::new(Some(session.clone().into()), sink.clone());
        let mut delivered_counts = HashMap::new();
        let oversized_continuation = mk_cont(&oversized, &session);
        let oversized_key = DelegationKey::from(&oversized_continuation);

        scheduler.push_continuation(oversized_continuation);
        for arrival in other_arrivals.iter().take(2) {
            scheduler.push_continuation(mk_cont(arrival, &session));
        }
        scheduler.push_user(user_input());

        let dispatched =
            drive_one_action(&mut scheduler, Instant::now(), false, &mut delivered_counts);
        prop_assert!(dispatched, "expected a merged prompt to dispatch");

        drain_to_quiescence(&mut scheduler, Instant::now(), &mut delivered_counts);

        let events = sink.snapshot();
        prop_assert_eq!(
            defer_count_for_key(&events, &oversized_key),
            0,
            "oversized continuation {:?} must not be deferred: {:?}",
            oversized_key,
            events,
        );
        prop_assert_eq!(
            oversized_drop_count_for_key(&events, &oversized_key),
            1,
            "oversized continuation {:?} must emit exactly one OversizedSingleItem drop: {:?}",
            oversized_key,
            events,
        );
    }
}
