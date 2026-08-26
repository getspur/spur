//! Lossless, origin-faithful records shared by memory benchmark adapters.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// A supported origin dataset.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    Locomo,
    LongMemEval,
}

/// Immutable origin and content pins for a benchmark source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePin {
    pub origin: String,
    pub revision: String,
    pub sha256: String,
}

/// One canonical dataset, including its untouched source-content digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkDataset {
    pub kind: DatasetKind,
    pub source: SourcePin,
    pub conversations: Vec<ConversationRecord>,
    pub questions: Vec<QuestionRecord>,
    pub raw_sha256: String,
}

impl BenchmarkDataset {
    /// Construct a canonical dataset and bind it to the exact input bytes.
    pub fn new(
        kind: DatasetKind,
        source: SourcePin,
        conversations: Vec<ConversationRecord>,
        questions: Vec<QuestionRecord>,
        raw_json: &str,
    ) -> Self {
        Self {
            kind,
            source,
            conversations,
            questions,
            raw_sha256: sha256_hex(raw_json.as_bytes()),
        }
    }

    /// Iterate over sessions in canonical conversation and occurrence order.
    pub fn all_sessions(&self) -> impl Iterator<Item = &SessionRecord> {
        self.conversations
            .iter()
            .flat_map(|conversation| conversation.sessions.iter())
    }

    /// Resolve a turn only by its occurrence-scoped internal identifier.
    pub fn turn(&self, internal_id: &str) -> Option<&TurnRecord> {
        self.all_sessions()
            .flat_map(|session| session.turns.iter())
            .find(|turn| turn.internal_id == internal_id)
    }
}

/// A source conversation containing sessions in their original order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationRecord {
    pub internal_id: String,
    pub source_id: Option<String>,
    pub sessions: Vec<SessionRecord>,
    pub raw: Value,
}

/// One occurrence of a source session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionRecord {
    pub internal_id: String,
    pub source_id: Option<String>,
    pub occurred_at: Option<String>,
    pub turns: Vec<TurnRecord>,
    pub raw: Value,
}

/// The normalized conversational role of a turn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Other,
}

/// One occurrence of a source turn with its complete original payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnRecord {
    pub internal_id: String,
    pub source_id: Option<String>,
    pub role: Role,
    pub speaker: Option<String>,
    pub content: String,
    pub caption: Option<String>,
    pub has_answer: Option<bool>,
    pub raw: Value,
}

/// An original evidence annotation and its optional canonical resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRef {
    pub raw: String,
    pub resolved_turn_id: Option<String>,
}

/// A benchmark question with independent session- and turn-level gold views.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionRecord {
    pub id: String,
    pub text: String,
    pub question_date: Option<String>,
    pub answer: Value,
    pub category: Option<u32>,
    pub question_type: Option<String>,
    pub evidence: Vec<EvidenceRef>,
    pub gold_session_ids: Vec<String>,
    pub gold_turn_ids: Vec<String>,
    pub raw: Value,
}

/// Build a stable internal ID for one occurrence of a source object.
///
/// The explicit occurrence index prevents a repeated source ID from becoming
/// canonical identity. Length-prefixed components avoid delimiter ambiguity.
pub fn occurrence_id(
    dataset_scope: &str,
    parent_scope: &str,
    occurrence_index: usize,
    source_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"spur-memory-occurrence-v1\0");
    hash_component(&mut hasher, dataset_scope.as_bytes());
    hash_component(&mut hasher, parent_scope.as_bytes());
    hasher.update((occurrence_index as u64).to_be_bytes());
    hash_component(&mut hasher, source_id.as_bytes());
    format!("occ_{:x}", hasher.finalize())
}

fn hash_component(hasher: &mut Sha256, component: &[u8]) {
    hasher.update((component.len() as u64).to_be_bytes());
    hasher.update(component);
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture_turn_with_caption_and_date() -> TurnRecord {
        TurnRecord {
            internal_id: occurrence_id("longmemeval-turn", "q1/session/0", 0, "turn-0"),
            source_id: Some("turn-0".to_owned()),
            role: Role::Assistant,
            speaker: Some("assistant".to_owned()),
            content: "line one\nline two".to_owned(),
            caption: Some("a blue bicycle".to_owned()),
            has_answer: Some(true),
            raw: json!({
                "role": "assistant",
                "content": "line one\nline two",
                "caption": "a blue bicycle",
                "has_answer": true
            }),
        }
    }

    fn fixture_dataset() -> BenchmarkDataset {
        let turn = fixture_turn_with_caption_and_date();
        let turn_id = turn.internal_id.clone();
        let session = SessionRecord {
            internal_id: occurrence_id("longmemeval-session", "q1", 0, "shared"),
            source_id: Some("shared".to_owned()),
            occurred_at: Some("2024-01-31".to_owned()),
            turns: vec![turn],
            raw: json!({"session_id": "shared", "date": "2024-01-31"}),
        };
        let session_id = session.internal_id.clone();
        BenchmarkDataset::new(
            DatasetKind::LongMemEval,
            SourcePin {
                origin: "huggingface://LongMemEval-S-cleaned".to_owned(),
                revision: "98d7416c24c778c2fee6e6f3006e7a073259d48f".to_owned(),
                sha256: "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
                    .to_owned(),
            },
            vec![ConversationRecord {
                internal_id: occurrence_id("longmemeval-conversation", "q1", 0, "q1"),
                source_id: Some("q1".to_owned()),
                sessions: vec![session],
                raw: json!({"question_id": "q1"}),
            }],
            vec![QuestionRecord {
                id: "q1".to_owned(),
                text: "What color was the bicycle?".to_owned(),
                question_date: Some("2024-02-01".to_owned()),
                answer: json!("blue"),
                category: None,
                question_type: Some("single-session-user".to_owned()),
                evidence: vec![EvidenceRef {
                    raw: "shared:turn-0".to_owned(),
                    resolved_turn_id: Some(turn_id.clone()),
                }],
                gold_session_ids: vec![session_id],
                gold_turn_ids: vec![turn_id],
                raw: json!({
                    "question_id": "q1",
                    "question_date": "2024-02-01",
                    "answer": "blue"
                }),
            }],
            "{}",
        )
    }

    #[test]
    fn occurrence_ids_are_deterministic_and_distinguish_repeated_source_sessions() {
        let first = occurrence_id("longmemeval", "q1", 0, "shared");
        assert_eq!(first, occurrence_id("longmemeval", "q1", 0, "shared"));
        assert_ne!(first, occurrence_id("longmemeval", "q1", 1, "shared"));
    }

    #[test]
    fn canonical_turn_keeps_raw_and_typed_content() {
        let turn = fixture_turn_with_caption_and_date();
        assert_eq!(turn.role, Role::Assistant);
        assert_eq!(turn.speaker.as_deref(), Some("assistant"));
        assert_eq!(turn.content, "line one\nline two");
        assert_eq!(turn.caption.as_deref(), Some("a blue bicycle"));
        assert_eq!(turn.has_answer, Some(true));
        assert!(turn.raw.get("content").is_some());
    }

    #[test]
    fn canonical_dataset_round_trip_preserves_dates_raw_and_both_gold_views() {
        let dataset = fixture_dataset();
        assert_eq!(
            dataset.raw_sha256,
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );

        let encoded = serde_json::to_value(&dataset).expect("serialize canonical dataset");
        let decoded: BenchmarkDataset =
            serde_json::from_value(encoded).expect("deserialize canonical dataset");
        assert_eq!(decoded, dataset);

        let question = &decoded.questions[0];
        assert_eq!(question.question_date.as_deref(), Some("2024-02-01"));
        assert_eq!(question.gold_session_ids.len(), 1);
        assert_eq!(question.gold_turn_ids.len(), 1);
        assert_eq!(
            decoded.conversations[0].sessions[0].occurred_at.as_deref(),
            Some("2024-01-31")
        );
    }

    #[test]
    fn dataset_accessors_resolve_internal_occurrence_ids() {
        let dataset = fixture_dataset();
        let question = &dataset.questions[0];
        let sessions = dataset.all_sessions().collect::<Vec<_>>();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].internal_id, question.gold_session_ids[0]);
        assert_eq!(
            dataset
                .turn(&question.gold_turn_ids[0])
                .map(|turn| turn.role),
            Some(Role::Assistant)
        );
        assert!(dataset.turn("shared").is_none());
    }
}
