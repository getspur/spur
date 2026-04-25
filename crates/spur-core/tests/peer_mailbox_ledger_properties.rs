use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId,
};
use spur_core::peer_mailbox::ledger::{LedgerError, TransitionOutcome};
use spur_core::peer_mailbox::{InMemoryLedger, PeerMailboxLedger};
use std::collections::HashSet;
use uuid::Uuid;

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
    "prompt-[a-f0-9]{8}"
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

fn non_terminal_states() -> [LedgerState; 5] {
    [
        LedgerState::Accepted,
        LedgerState::Queued,
        LedgerState::DeliveredInflight,
        LedgerState::Delivered,
        LedgerState::Unknown,
    ]
}

fn is_terminal_local(state: LedgerState) -> bool {
    matches!(
        state,
        LedgerState::Rejected
            | LedgerState::Consumed
            | LedgerState::Ignored
            | LedgerState::Expired
            | LedgerState::Dropped
            | LedgerState::Undeliverable
    )
}

fn is_valid_transition_local(from: LedgerState, to: LedgerState) -> bool {
    if from == to {
        return true;
    }

    matches!(
        (from, to),
        (
            LedgerState::Accepted,
            LedgerState::Queued
                | LedgerState::DeliveredInflight
                | LedgerState::Undeliverable
                | LedgerState::Consumed
                | LedgerState::Ignored
        ) | (
            LedgerState::Queued,
            LedgerState::DeliveredInflight
                | LedgerState::Expired
                | LedgerState::Dropped
                | LedgerState::Undeliverable
        ) | (
            LedgerState::DeliveredInflight,
            LedgerState::Queued
                | LedgerState::Delivered
                | LedgerState::Consumed
                | LedgerState::Ignored
                | LedgerState::Expired
                | LedgerState::Dropped
        ) | (
            LedgerState::Delivered,
            LedgerState::Consumed
                | LedgerState::Ignored
                | LedgerState::Expired
                | LedgerState::Dropped
        )
    )
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
        terminal: is_terminal_local(entry.state),
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
        _ => panic!("unsupported non-exhaustive ledger state {state:?}"),
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
    if is_terminal_local(current) {
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
    assert!(is_terminal_local(terminal));
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
    } else if is_valid_transition_local(from, to) {
        TransitionSignature::Changed { from, to }
    } else {
        TransitionSignature::InvalidTransition { from, to }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn prop_replay_idempotence(ops in prop::collection::vec(op_strategy(), 0..50)) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let envelope = mk_envelope(1);
            let ledger_a = InMemoryLedger::new();
            apply_ops(&ledger_a, &envelope, &ops).await;
            let snapshot_a = snapshot(&ledger_a, &envelope.message_id).await;

            let ledger_b = InMemoryLedger::new();
            apply_ops(&ledger_b, &envelope, &ops).await;
            let snapshot_b = snapshot(&ledger_b, &envelope.message_id).await;

            assert_eq!(snapshot_a, snapshot_b);
        });
    }

    #[test]
    fn prop_terminal_states_reject_outgoing_transitions(
        ops in prop::collection::vec(op_strategy(), 0..50),
        terminal in reachable_terminal_strategy(),
    ) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let envelope = mk_envelope(2);
            let ledger = InMemoryLedger::new();
            apply_ops(&ledger, &envelope, &ops).await;
            let terminal = ensure_terminal(&ledger, &envelope, terminal).await;

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
    fn prop_transition_matrix_consistency(
        from in reachable_state_strategy(),
        to in ledger_state_strategy(),
        prompts_a in prop::collection::vec(prompt_id_strategy(), 0..8),
        prompts_b in prop::collection::vec(prompt_id_strategy(), 0..8),
    ) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
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
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let envelope = mk_envelope(5);
            let ledger = InMemoryLedger::new();
            let mut previous_len = 0;

            for op in &ops {
                apply_op(&ledger, &envelope, op).await;
                if let Some(entry) = ledger.get(&envelope.message_id).await {
                    let current_len = entry.injected_into_prompts.len();
                    assert!(
                        current_len >= previous_len,
                        "injection set shrank from {previous_len} to {current_len}"
                    );
                    previous_len = current_len;
                }
            }
        });
    }

    #[test]
    fn prop_terminal_lockout_holds_under_relaxed_matrix(
        terminal in reachable_terminal_strategy(),
    ) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let envelope = mk_envelope(6);
            let ledger = InMemoryLedger::new();
            ledger.accept(envelope.clone()).await.unwrap();
            drive_to_state(&ledger, &envelope.message_id, terminal).await;

            for target in non_terminal_states() {
                let signature = transition_signature(
                    ledger.transition(&envelope.message_id, target).await
                );
                assert_eq!(
                    signature,
                    TransitionSignature::InvalidTransition {
                        from: terminal,
                        to: target,
                    }
                );
            }
        });
    }

    #[test]
    fn prop_accept_then_transition_invariants(seed in any::<u64>()) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
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
}
