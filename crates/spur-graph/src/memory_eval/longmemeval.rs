use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use super::{
    contract::{
        occurrence_id, BenchmarkDataset, ConversationRecord, DatasetKind, QuestionRecord, Role,
        SessionRecord, SourcePin, TurnRecord,
    },
    graphify_slice, EvalSplit, MemoryTask, LME_GRAPHIFY_N,
};

#[derive(Debug, Deserialize)]
struct LongMemEvalItem {
    question_id: String,
    question: String,
    #[serde(default)]
    answer: serde_json::Value,
    #[serde(default)]
    answer_session_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CanonicalLongMemEvalItem {
    question_id: String,
    question_type: String,
    question: String,
    answer: Value,
    #[serde(default)]
    question_date: Option<String>,
    haystack_session_ids: Vec<String>,
    haystack_dates: Vec<String>,
    haystack_sessions: Vec<Vec<Value>>,
    answer_session_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CanonicalLongMemEvalTurn {
    role: String,
    content: String,
    #[serde(default)]
    has_answer: Option<bool>,
}

#[derive(Debug, Error)]
enum LongMemEvalParseError {
    #[error(
        "parallel haystack arrays differ for {question_id}: ids={ids} dates={dates} sessions={sessions}"
    )]
    ParallelHaystackArrays {
        question_id: String,
        ids: usize,
        dates: usize,
        sessions: usize,
    },
}

impl BenchmarkDataset {
    /// Decode LongMemEval without dropping abstention rows or conflating source
    /// session provenance with occurrence identity.
    pub fn load_longmemeval(json: &str, source: SourcePin) -> anyhow::Result<Self> {
        let raw_items: Vec<Value> = serde_json::from_str(json)?;
        let mut conversations = Vec::with_capacity(raw_items.len());
        let mut questions = Vec::with_capacity(raw_items.len());

        for (question_index, raw_item) in raw_items.into_iter().enumerate() {
            let item: CanonicalLongMemEvalItem = serde_json::from_value(raw_item.clone())
                .with_context(|| format!("invalid LongMemEval row at index {question_index}"))?;
            ensure_parallel_haystacks(&item)?;

            let mut sessions = Vec::with_capacity(item.haystack_sessions.len());
            let mut gold_turn_ids = Vec::new();
            for session_index in 0..item.haystack_sessions.len() {
                let source_session_id = &item.haystack_session_ids[session_index];
                let internal_id = occurrence_id(
                    "longmemeval-session",
                    &item.question_id,
                    session_index,
                    source_session_id,
                );
                let raw_turns = &item.haystack_sessions[session_index];
                let mut turns = Vec::with_capacity(raw_turns.len());

                for (turn_index, raw_turn) in raw_turns.iter().enumerate() {
                    let turn: CanonicalLongMemEvalTurn = serde_json::from_value(raw_turn.clone())
                        .with_context(|| {
                        format!(
                            "invalid LongMemEval turn for {} session {} turn {}",
                            item.question_id, session_index, turn_index
                        )
                    })?;
                    let turn_internal_id = occurrence_id(
                        "longmemeval-turn",
                        &internal_id,
                        turn_index,
                        source_session_id,
                    );
                    if turn.has_answer == Some(true) {
                        gold_turn_ids.push(turn_internal_id.clone());
                    }
                    turns.push(TurnRecord {
                        internal_id: turn_internal_id,
                        source_id: None,
                        role: parse_role(&turn.role),
                        speaker: None,
                        content: turn.content,
                        caption: None,
                        has_answer: turn.has_answer,
                        raw: raw_turn.clone(),
                    });
                }

                sessions.push(SessionRecord {
                    internal_id,
                    source_id: Some(source_session_id.clone()),
                    occurred_at: Some(item.haystack_dates[session_index].clone()),
                    turns,
                    raw: Value::Array(raw_turns.clone()),
                });
            }

            let gold_session_ids = resolve_session_gold(&item.answer_session_ids, &sessions);
            let conversation_internal_id = occurrence_id(
                "longmemeval-conversation",
                "dataset",
                question_index,
                &item.question_id,
            );
            conversations.push(ConversationRecord {
                internal_id: conversation_internal_id,
                source_id: Some(item.question_id.clone()),
                sessions,
                raw: raw_item.clone(),
            });
            questions.push(QuestionRecord {
                id: item.question_id,
                text: item.question,
                question_date: item.question_date,
                answer: item.answer,
                category: None,
                question_type: Some(item.question_type),
                evidence: Vec::new(),
                gold_session_ids,
                gold_turn_ids,
                raw: raw_item,
            });
        }

        Ok(BenchmarkDataset::new(
            DatasetKind::LongMemEval,
            source,
            conversations,
            questions,
            json,
        ))
    }
}

fn ensure_parallel_haystacks(item: &CanonicalLongMemEvalItem) -> anyhow::Result<()> {
    let ids = item.haystack_session_ids.len();
    let dates = item.haystack_dates.len();
    let sessions = item.haystack_sessions.len();
    if ids != dates || dates != sessions {
        return Err(LongMemEvalParseError::ParallelHaystackArrays {
            question_id: item.question_id.clone(),
            ids,
            dates,
            sessions,
        }
        .into());
    }
    Ok(())
}

fn parse_role(role: &str) -> Role {
    match role {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => Role::Other,
    }
}

fn resolve_session_gold(answer_session_ids: &[String], sessions: &[SessionRecord]) -> Vec<String> {
    answer_session_ids
        .iter()
        .flat_map(|answer_session_id| {
            sessions.iter().filter_map(move |session| {
                (session.source_id.as_deref() == Some(answer_session_id.as_str()))
                    .then(|| session.internal_id.clone())
            })
        })
        .collect()
}

pub fn parse_longmemeval(json: &str, split: EvalSplit) -> anyhow::Result<Vec<MemoryTask>> {
    let items: Vec<LongMemEvalItem> = serde_json::from_str(json)?;
    let mut tasks = Vec::new();
    for item in items {
        if item.question_id.ends_with("_abs") {
            continue;
        }
        if item.answer_session_ids.is_empty() {
            continue;
        }
        tasks.push(MemoryTask {
            id: item.question_id,
            question: item.question,
            gold_ids: item.answer_session_ids,
            gold_answer: super::stringify_answer(&item.answer),
        });
    }
    Ok(match split {
        EvalSplit::Official => tasks,
        EvalSplit::Graphify => graphify_slice(&tasks, LME_GRAPHIFY_N).to_vec(),
    })
}
