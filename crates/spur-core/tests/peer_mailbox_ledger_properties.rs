use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId,
};
use spur_core::peer_mailbox::ledger::{
    is_terminal, is_valid_transition, InjectionOutcome, LedgerError, TransitionOutcome,
};
use spur_core::peer_mailbox::{InMemoryLedger, PeerMailboxLedger};
use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::runtime::{Builder, Runtime};
use uuid::Uuid;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds")
});

#[derive(Debug, Clone)]
enum Op {
    Accept,
    TransitionTo(LedgerState),
    RecordInjection(String),
    Get,
}

#[derive(Debug, PartialEq, Eq)]
struct EntrySnapshot {
    state: LedgerState,
    injected_into_prompts: HashSet<String>,
    terminal: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum TransitionSignature {
    Changed { from: LedgerState, to: LedgerState },
    Unchanged(LedgerState),
    InvalidTransition { from: LedgerState, to: LedgerState },
    NotFound,
    AlreadyTerminal(LedgerState),
}

pub fn ledger_state_strategy() -> impl Strategy<Value = LedgerState> {
    prop_oneof![
        Just(LedgerState::Accepted),
        Just(LedgerState::Rejected),
        Just(LedgerState::Queued),
        Just(LedgerState::DeliveredInflight),
        Just(LedgerState::Delivered),
        Just(LedgerState::Consumed),
        Just(LedgerState::Ignored),
        Just(LedgerState::Expired),
        Just(LedgerState::Dropped),
        Just(LedgerState::Undeliverable),
        Just(LedgerState::Unknown),
    ]
}

fn reachable_state_strategy() -> impl Strategy<Value = LedgerState> {
    prop_oneof![
        Just(LedgerState::Accepted),
        Just(LedgerState::Queued),
        Just(LedgerState::DeliveredInflight),
        Just(LedgerState::Delivered),
        Just(LedgerState::Consumed),
        Just(LedgerState::Ignored),
        Just(LedgerState::Expired),
        Just(LedgerState::Dropped),
        Just(LedgerState::Undeliverable),
    ]
}

fn reachable_terminal_strategy() -> impl Strategy<Value = LedgerState> {
    prop_oneof![
        Just(LedgerState::Consumed),
        Just(LedgerState::Ignored),
        Just(LedgerState::Expired),
        Just(LedgerState::Dropped),
        Just(LedgerState::Undeliverable),
    ]
}

fn prompt_id_strategy() -> impl Strategy<Value = String> {
    "prompt-[a-f0-9]{1}"
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        1 => Just(Op::Accept),
        5 => ledger_state_strategy().prop_map(Op::TransitionTo),
        2 => prompt_id_strategy().prop_map(Op::RecordInjection),
        1 => Just(Op::Get),
    ]
}

fn all_states() -> [LedgerState; 11] {
    [
        LedgerState::Accepted,
        LedgerState::Rejected,
        LedgerState::Queued,
        LedgerState::DeliveredInflight,
        LedgerState::Delivered,
        LedgerState::Consumed,
        LedgerState::Ignored,
        LedgerState::Expired,
        LedgerState::Dropped,
        LedgerState::Undeliverable,
        LedgerState::Unknown,
    ]
}

fn mk_envelope(message_id_seed: u64) -> PeerMessageEnvelope {
    PeerMessageEnvelope {
        schema: "spur-peer-message/v1".into(),
        message_id: PeerMessageId(Uuid::from_u128(message_id_seed as u128)),
        source_delegation_id: DelegationId(format!("src-{message_id_seed}")),
        target_delegation_id: DelegationId(format!("tgt-{message_id_seed}")),
        source_issue_id: format!("source-issue-{message_id_seed}"),
        target_issue_id: format!("target-issue-{message_id_seed}"),
        source_plan_task_id: format!("source-task-{message_id_seed}"),
        target_plan_task_id: format!("target-task-{message_id_seed}"),
        source_executor_id: format!("executor-{message_id_seed}"),
        plan_version: 1,
        kind: MessageKind::Handoff,
        body: format!("peer mailbox property message {message_id_seed}"),
        sequence: message_id_seed,
    }
}

async fn apply_op(ledger: &InMemoryLedger, envelope: &PeerMessageEnvelope, op: &Op) {
    match op {
        Op::Accept => {
            let _ = ledger.accept(envelope.clone()).await;
        }
        Op::TransitionTo(next) => {
            let _ = ledger.transition(&envelope.message_id, *next).await;
        }
        Op::RecordInjection(prompt_id) => {
            let _ = ledger
                .record_injection(&envelope.message_id, prompt_id)
                .await;
        }
        Op::Get => {
            let _ = ledger.get(&envelope.message_id).await;
        }
    }
}

async fn apply_ops(ledger: &InMemoryLedger, envelope: &PeerMessageEnvelope, ops: &[Op]) {
    for op in ops {
        apply_op(ledger, envelope, op).await;
    }
}

async fn snapshot(ledger: &InMemoryLedger, message_id: &PeerMessageId) -> Option<EntrySnapshot> {
    ledger.get(message_id).await.map(|entry| EntrySnapshot {
        state: entry.state,
        injected_into_prompts: entry.injected_into_prompts,
        terminal: is_terminal(entry.state),
    })
}

async fn drive_to_state(ledger: &InMemoryLedger, message_id: &PeerMessageId, state: LedgerState) {
    match state {
        LedgerState::Accepted => {}
        LedgerState::Queued => {
            ledger
                .transition(message_id, LedgerState::Queued)
                .await
                .unwrap();
        }
        LedgerState::DeliveredInflight => {
            ledger
                .transition(message_id, LedgerState::DeliveredInflight)
                .await
                .unwrap();
        }
        LedgerState::Delivered => {
            ledger
                .transition(message_id, LedgerState::DeliveredInflight)
                .await
                .unwrap();
            ledger
                .transition(message_id, LedgerState::Delivered)
                .await
                .unwrap();
        }
        LedgerState::Consumed => {
            ledger
                .transition(message_id, LedgerState::Consumed)
                .await
                .unwrap();
        }
        LedgerState::Ignored => {
            ledger
                .transition(message_id, LedgerState::Ignored)
                .await
                .unwrap();
        }
        LedgerState::Expired => {
            ledger
                .transition(message_id, LedgerState::Queued)
                .await
                .unwrap();
            ledger
                .transition(message_id, LedgerState::Expired)
                .await
                .unwrap();
        }
        LedgerState::Dropped => {
            ledger
                .transition(message_id, LedgerState::Queued)
                .await
                .unwrap();
            ledger
                .transition(message_id, LedgerState::Dropped)
                .await
                .unwrap();
        }
        LedgerState::Undeliverable => {
            ledger
                .transition(message_id, LedgerState::Undeliverable)
                .await
                .unwrap();
        }
        LedgerState::Rejected | LedgerState::Unknown => {
            panic!("{state:?} is not reachable through the public ledger API")
        }
        future_state => {
            panic!("unsupported future ledger state {future_state:?}")
        }
    }
}

async fn ensure_terminal(
    ledger: &InMemoryLedger,
    envelope: &PeerMessageEnvelope,
    preferred_terminal: LedgerState,
) -> LedgerState {
    if ledger.get(&envelope.message_id).await.is_none() {
        ledger.accept(envelope.clone()).await.unwrap();
    }

    let current = ledger.get(&envelope.message_id).await.unwrap().state;
    if is_terminal(current) {
        return current;
    }

    match current {
        LedgerState::Accepted => {
            drive_to_state(ledger, &envelope.message_id, preferred_terminal).await;
        }
        LedgerState::Queued => match preferred_terminal {
            LedgerState::Consumed | LedgerState::Ignored => {
                ledger
                    .transition(&envelope.message_id, LedgerState::DeliveredInflight)
                    .await
                    .unwrap();
                ledger
                    .transition(&envelope.message_id, preferred_terminal)
                    .await
                    .unwrap();
            }
            _ => {
                ledger
                    .transition(&envelope.message_id, preferred_terminal)
                    .await
                    .unwrap();
            }
        },
        LedgerState::DeliveredInflight => match preferred_terminal {
            LedgerState::Undeliverable => {
                ledger
                    .transition(&envelope.message_id, LedgerState::Queued)
                    .await
                    .unwrap();
                ledger
                    .transition(&envelope.message_id, LedgerState::Undeliverable)
                    .await
                    .unwrap();
            }
            _ => {
                ledger
                    .transition(&envelope.message_id, preferred_terminal)
                    .await
                    .unwrap();
            }
        },
        LedgerState::Delivered => {
            let terminal = if preferred_terminal == LedgerState::Undeliverable {
                LedgerState::Consumed
            } else {
                preferred_terminal
            };
            ledger
                .transition(&envelope.message_id, terminal)
                .await
                .unwrap();
        }
        other => panic!("unexpected non-terminal state {other:?}"),
    }

    let terminal = ledger.get(&envelope.message_id).await.unwrap().state;
    assert!(is_terminal(terminal));
    terminal
}

fn transition_signature(result: Result<TransitionOutcome, LedgerError>) -> TransitionSignature {
    match result {
        Ok(TransitionOutcome::Changed { from, to }) => TransitionSignature::Changed { from, to },
        Ok(TransitionOutcome::Unchanged(state)) => TransitionSignature::Unchanged(state),
        Err(LedgerError::InvalidTransition { from, to }) => {
            TransitionSignature::InvalidTransition { from, to }
        }
        Err(LedgerError::NotFound(_)) => TransitionSignature::NotFound,
        Err(LedgerError::AlreadyTerminal { state, .. }) => {
            TransitionSignature::AlreadyTerminal(state)
        }
    }
}

fn expected_transition_signature(from: LedgerState, to: LedgerState) -> TransitionSignature {
    if from == to {
        TransitionSignature::Unchanged(to)
    } else if is_valid_transition(from, to) {
        TransitionSignature::Changed { from, to }
    } else {
        TransitionSignature::InvalidTransition { from, to }
    }
}

/// Covers the current in-memory ledger; Stage-2 persistent ledger impls must
/// re-verify this property when they exist.
#[test]
fn prop_apply_is_deterministic() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    });

    runner
        .run(&prop::collection::vec(op_strategy(), 0..50), |ops| {
            RUNTIME.block_on(async {
                let envelope = mk_envelope(1);
                let ledger_a = InMemoryLedger::new();
                apply_ops(&ledger_a, &envelope, &ops).await;
                let snapshot_a = snapshot(&ledger_a, &envelope.message_id).await;

                let ledger_b = InMemoryLedger::new();
                apply_ops(&ledger_b, &envelope, &ops).await;
                let snapshot_b = snapshot(&ledger_b, &envelope.message_id).await;

                assert_eq!(snapshot_a, snapshot_b);
            });
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_relaxed_arms_are_legal() {
    RUNTIME.block_on(async {
        for (idx, from, to) in [
            (0, LedgerState::Accepted, LedgerState::Consumed),
            (1, LedgerState::Accepted, LedgerState::Ignored),
            (2, LedgerState::DeliveredInflight, LedgerState::Consumed),
            (3, LedgerState::DeliveredInflight, LedgerState::Ignored),
            (4, LedgerState::DeliveredInflight, LedgerState::Expired),
            (5, LedgerState::DeliveredInflight, LedgerState::Dropped),
        ] {
            let envelope = mk_envelope(70 + idx);
            let ledger = InMemoryLedger::new();
            ledger.accept(envelope.clone()).await.unwrap();
            drive_to_state(&ledger, &envelope.message_id, from).await;

            assert_eq!(
                ledger.transition(&envelope.message_id, to).await.unwrap(),
                TransitionOutcome::Changed { from, to }
            );
        }

        let envelope = mk_envelope(80);
        let ledger = InMemoryLedger::new();
        ledger.accept(envelope.clone()).await.unwrap();
        drive_to_state(&ledger, &envelope.message_id, LedgerState::Queued).await;

        assert_eq!(
            transition_signature(
                ledger
                    .transition(&envelope.message_id, LedgerState::Consumed)
                    .await
            ),
            TransitionSignature::InvalidTransition {
                from: LedgerState::Queued,
                to: LedgerState::Consumed,
            }
        );
    });
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn prop_apply_is_idempotent_for_terminal_state(
        ops in prop::collection::vec(op_strategy(), 0..50),
        terminal in reachable_terminal_strategy(),
    ) {
        RUNTIME.block_on(async {
            let envelope = mk_envelope(2);
            let ledger = InMemoryLedger::new();
            apply_ops(&ledger, &envelope, &ops).await;
            let terminal = ensure_terminal(&ledger, &envelope, terminal).await;
            let state_before = ledger.get(&envelope.message_id).await.unwrap().state;

            apply_ops(&ledger, &envelope, &ops).await;

            let state_after = ledger.get(&envelope.message_id).await.unwrap().state;
            assert_eq!(state_before, terminal);
            assert_eq!(state_after, terminal);
        });
    }

    #[test]
    fn prop_transition_matrix_consistency(
        from in reachable_state_strategy(),
        to in ledger_state_strategy(),
        prompts_a in prop::collection::vec(prompt_id_strategy(), 0..8),
        prompts_b in prop::collection::vec(prompt_id_strategy(), 0..8),
    ) {
        RUNTIME.block_on(async {
            let envelope_a = mk_envelope(3);
            let ledger_a = InMemoryLedger::new();
            ledger_a.accept(envelope_a.clone()).await.unwrap();
            for prompt in &prompts_a {
                ledger_a
                    .record_injection(&envelope_a.message_id, prompt)
                    .await
                    .unwrap();
            }
            drive_to_state(&ledger_a, &envelope_a.message_id, from).await;

            let envelope_b = mk_envelope(4);
            let ledger_b = InMemoryLedger::new();
            ledger_b.accept(envelope_b.clone()).await.unwrap();
            drive_to_state(&ledger_b, &envelope_b.message_id, from).await;
            for prompt in &prompts_b {
                ledger_b
                    .record_injection(&envelope_b.message_id, prompt)
                    .await
                    .unwrap();
            }

            let signature_a = transition_signature(
                ledger_a.transition(&envelope_a.message_id, to).await
            );
            let signature_b = transition_signature(
                ledger_b.transition(&envelope_b.message_id, to).await
            );
            let expected = expected_transition_signature(from, to);

            assert_eq!(signature_a, signature_b);
            assert_eq!(signature_a, expected);
        });
    }

    #[test]
    fn prop_injection_set_grows_monotonically(ops in prop::collection::vec(op_strategy(), 0..50)) {
        let (injected_count, already_injected_count) = RUNTIME.block_on(async {
            let envelope = mk_envelope(5);
            let ledger = InMemoryLedger::new();
            let mut previous_set = HashSet::new();
            let mut injected_count = 0;
            let mut already_injected_count = 0;

            for op in &ops {
                match op {
                    Op::RecordInjection(prompt_id) => {
                        match ledger.record_injection(&envelope.message_id, prompt_id).await {
                            Ok(InjectionOutcome::Injected) => injected_count += 1,
                            Ok(InjectionOutcome::AlreadyInjected) => already_injected_count += 1,
                            Err(LedgerError::NotFound(_)) => {}
                            Err(other) => panic!("unexpected injection error: {other:?}"),
                        }
                    }
                    other => apply_op(&ledger, &envelope, other).await,
                }

                if let Some(entry) = ledger.get(&envelope.message_id).await {
                    let current_set = entry.injected_into_prompts.clone();
                    assert!(
                        current_set.is_superset(&previous_set),
                        "injection set lost prompts: previous={previous_set:?}, current={current_set:?}"
                    );
                    previous_set = current_set;
                }
            }

            (injected_count, already_injected_count)
        });

        prop_assume!(injected_count > 0 && already_injected_count > 0);
    }

    #[test]
    fn prop_terminal_states_reject_outgoing_transitions_under_full_state_targeting(
        terminal in reachable_terminal_strategy(),
    ) {
        RUNTIME.block_on(async {
            let envelope = mk_envelope(6);
            let ledger = InMemoryLedger::new();
            ledger.accept(envelope.clone()).await.unwrap();
            drive_to_state(&ledger, &envelope.message_id, terminal).await;

            for target in all_states() {
                let signature = transition_signature(
                    ledger.transition(&envelope.message_id, target).await
                );
                if target == terminal {
                    assert_eq!(signature, TransitionSignature::Unchanged(terminal));
                } else {
                    assert_eq!(
                        signature,
                        TransitionSignature::InvalidTransition {
                            from: terminal,
                            to: target,
                        }
                    );
                }
            }
        });
    }

    #[test]
    fn prop_accept_invariants(seed in any::<u64>()) {
        RUNTIME.block_on(async {
            let envelope = mk_envelope(seed);
            let ledger = InMemoryLedger::new();
            ledger.accept(envelope.clone()).await.unwrap();

            let entry = ledger.get(&envelope.message_id).await.unwrap();
            assert_eq!(entry.state, LedgerState::Accepted);
            assert!(entry.injected_into_prompts.is_empty());
            assert!(ledger.get(&envelope.message_id).await.is_some());

            let non_terminal_ids: HashSet<_> = ledger
                .non_terminal_entries()
                .await
                .into_iter()
                .map(|entry| entry.envelope.message_id)
                .collect();
            assert!(non_terminal_ids.contains(&envelope.message_id));

            let pending_ids: HashSet<_> = ledger
                .pending_for_target(&envelope.target_delegation_id)
                .await
                .into_iter()
                .map(|entry| entry.envelope.message_id)
                .collect();
            assert!(pending_ids.contains(&envelope.message_id));
        });
    }

    #[test]
    fn prop_message_leaves_non_terminal_index_when_terminalized(
        seed in any::<u64>(),
        terminal in reachable_terminal_strategy(),
    ) {
        RUNTIME.block_on(async {
            let envelope = mk_envelope(seed);
            let ledger = InMemoryLedger::new();
            ledger.accept(envelope.clone()).await.unwrap();
            drive_to_state(&ledger, &envelope.message_id, terminal).await;

            let non_terminal_ids: HashSet<_> = ledger
                .non_terminal_entries()
                .await
                .into_iter()
                .map(|entry| entry.envelope.message_id)
                .collect();
            assert!(!non_terminal_ids.contains(&envelope.message_id));

            let pending_ids: HashSet<_> = ledger
                .pending_for_target(&envelope.target_delegation_id)
                .await
                .into_iter()
                .map(|entry| entry.envelope.message_id)
                .collect();
            assert!(!pending_ids.contains(&envelope.message_id));
        });
    }
}
