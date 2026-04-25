use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::delegation::DelegationId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerMessageId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMessageEnvelope {
    pub schema: String,
    pub message_id: PeerMessageId,
    pub source_delegation_id: DelegationId,
    pub target_delegation_id: DelegationId,
    pub source_issue_id: String,
    pub target_issue_id: String,
    pub source_plan_task_id: String,
    pub target_plan_task_id: String,
    pub source_executor_id: String,
    pub plan_version: u64,
    pub kind: MessageKind,
    pub body: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageKind {
    Question,
    Answer,
    Handoff,
    Warning,
    Constraint,
    /// Forward-compat fallback for unknown variants on the wire (e.g. a v2
    /// binary writing a kind that this binary doesn't recognize). Routes get
    /// rejected with `unsupported_message_kind` rather than failing JSON parse.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LedgerState {
    Accepted,
    Rejected,
    Queued,
    DeliveredInflight,
    Delivered,
    Consumed,
    Ignored,
    Expired,
    Dropped,
    Undeliverable,
    /// Forward-compat fallback for unknown ledger states. Reading a future
    /// log on this binary deserializes the unknown discriminant here rather
    /// than failing JSON parse.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TerminalOutcome {
    Consumed,
    Ignored { reason: String },
    Expired,
    Dropped { reason: String },
    Undeliverable { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrips_through_serde_json() {
        let envelope = PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId("src-1".into()),
            target_delegation_id: DelegationId("tgt-1".into()),
            source_issue_id: "bd-100".into(),
            target_issue_id: "bd-200".into(),
            source_plan_task_id: "task-a".into(),
            target_plan_task_id: "task-b".into(),
            source_executor_id: "exec-x".into(),
            plan_version: 7,
            kind: MessageKind::Handoff,
            body: "Done with reqwest probe; B to wire timeout".into(),
            sequence: 1,
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let back: PeerMessageEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope, back);
    }

    #[test]
    fn ledger_state_serializes_snake_case() {
        let s = serde_json::to_string(&LedgerState::DeliveredInflight).unwrap();
        assert_eq!(s, "\"delivered_inflight\"");
    }

    #[test]
    fn ledger_state_round_trips_snake_case() {
        for state in [
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
        ] {
            let s = serde_json::to_string(&state).unwrap();
            let back: LedgerState = serde_json::from_str(&s).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn unknown_message_kind_deserializes_to_unknown_variant() {
        let kind: MessageKind = serde_json::from_str("\"future_kind_v2\"").unwrap();
        assert_eq!(kind, MessageKind::Unknown);
    }

    #[test]
    fn unknown_ledger_state_deserializes_to_unknown_variant() {
        let state: LedgerState = serde_json::from_str("\"future_state_v2\"").unwrap();
        assert_eq!(state, LedgerState::Unknown);
    }

    #[test]
    fn peer_message_id_is_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(PeerMessageId(Uuid::new_v4()));
        assert_eq!(set.len(), 1);
    }
}
