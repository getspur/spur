use std::collections::HashMap;

use anyhow::{bail, Context};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::contract::{
    occurrence_id, BenchmarkDataset, ConversationRecord, DatasetKind, EvidenceRef, QuestionRecord,
    Role, SessionRecord, SourcePin, TurnRecord,
};
use super::{
    graphify_slice, EvalSplit, MemoryTask, LOCOMO_ADVERSARIAL_CATEGORY, LOCOMO_GRAPHIFY_N,
};

#[derive(Debug, Deserialize)]
struct LocomoSample {
    sample_id: String,
    conversation: Map<String, Value>,
    qa: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct LegacyLocomoSample {
    sample_id: String,
    qa: Vec<LocomoQa>,
}

#[derive(Debug)]
struct DecodedSample {
    sample_id: String,
    conversation: Map<String, Value>,
    qa: Vec<Value>,
    raw: Value,
}

#[derive(Debug, Deserialize)]
struct LocomoQa {
    question: String,
    #[serde(default)]
    answer: Value,
    category: u32,
    #[serde(default)]
    evidence: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LocomoTurn {
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    dia_id: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    blip_caption: Option<String>,
}

#[derive(Debug, Clone)]
struct TurnLocation {
    turn_id: String,
    session_id: String,
}

impl BenchmarkDataset {
    /// Decode LoCoMo into the canonical, lossless benchmark contract.
    pub fn load_locomo(json: &str, source: SourcePin) -> anyhow::Result<Self> {
        load_locomo(json, source)
    }
}

/// Decode LoCoMo into the canonical, lossless benchmark contract.
///
/// This layer preserves every source row. Retrieval eligibility is evaluated
/// later from the retained evidence-resolution state.
fn load_locomo(json: &str, source: SourcePin) -> anyhow::Result<BenchmarkDataset> {
    let raw: Value = serde_json::from_str(json).context("parse LoCoMo JSON")?;
    let samples = decode_samples(&raw)?;
    let mut conversations = Vec::with_capacity(samples.len());
    let mut questions = Vec::new();

    for (sample_index, sample) in samples.iter().enumerate() {
        let (conversation, evidence_index) = canonical_conversation(sample, sample_index)?;
        questions.extend(canonical_questions(sample, &evidence_index)?);
        conversations.push(conversation);
    }

    Ok(BenchmarkDataset::new(
        DatasetKind::Locomo,
        source,
        conversations,
        questions,
        json,
    ))
}

fn decode_samples(raw: &Value) -> anyhow::Result<Vec<DecodedSample>> {
    let raw_samples = raw
        .as_array()
        .context("LoCoMo root must be an array of samples")?;

    raw_samples
        .iter()
        .enumerate()
        .map(|(sample_index, raw_sample)| {
            let sample: LocomoSample = serde_json::from_value(raw_sample.clone())
                .with_context(|| format!("decode LoCoMo sample {sample_index}"))?;
            Ok(DecodedSample {
                sample_id: sample.sample_id,
                conversation: sample.conversation,
                qa: sample.qa,
                raw: raw_sample.clone(),
            })
        })
        .collect()
}

fn canonical_conversation(
    sample: &DecodedSample,
    sample_index: usize,
) -> anyhow::Result<(ConversationRecord, HashMap<String, Vec<TurnLocation>>)> {
    let conversation_id = occurrence_id(
        "locomo-conversation",
        "root",
        sample_index,
        &sample.sample_id,
    );
    let mut session_entries: Vec<(usize, &String, &Value)> = sample
        .conversation
        .iter()
        .filter_map(|(key, value)| session_number(key).map(|number| (number, key, value)))
        .collect();
    session_entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));

    let mut sessions = Vec::with_capacity(session_entries.len());
    let mut evidence_index: HashMap<String, Vec<TurnLocation>> = HashMap::new();
    for (session_index, (_, session_key, raw_session)) in session_entries.into_iter().enumerate() {
        let source_turns = raw_session.as_array().with_context(|| {
            format!(
                "LoCoMo sample {} field {session_key} must be an array",
                sample.sample_id
            )
        })?;
        let session_id = occurrence_id(
            "locomo-session",
            &conversation_id,
            session_index,
            session_key,
        );
        let occurred_at = session_date(&sample.conversation, session_key, &sample.sample_id)?;
        let mut turns = Vec::with_capacity(source_turns.len());

        for (turn_index, raw_turn) in source_turns.iter().enumerate() {
            let turn: LocomoTurn = serde_json::from_value(raw_turn.clone()).with_context(|| {
                format!(
                    "decode LoCoMo sample {} {session_key} turn {turn_index}",
                    sample.sample_id
                )
            })?;
            let turn_id = occurrence_id(
                "locomo-turn",
                &session_id,
                turn_index,
                turn.dia_id.as_deref().unwrap_or(""),
            );
            if let Some(source_id) = &turn.dia_id {
                evidence_index
                    .entry(source_id.clone())
                    .or_default()
                    .push(TurnLocation {
                        turn_id: turn_id.clone(),
                        session_id: session_id.clone(),
                    });
            }
            turns.push(TurnRecord {
                internal_id: turn_id,
                source_id: turn.dia_id,
                role: Role::Other,
                speaker: turn.speaker,
                content: turn.text,
                caption: turn.blip_caption,
                has_answer: None,
                raw: raw_turn.clone(),
            });
        }

        sessions.push(SessionRecord {
            internal_id: session_id,
            source_id: Some(session_key.clone()),
            occurred_at,
            turns,
            raw: raw_session.clone(),
        });
    }

    Ok((
        ConversationRecord {
            internal_id: conversation_id,
            source_id: Some(sample.sample_id.clone()),
            sessions,
            // The source has sample-level observation and summary fields but
            // no separate canonical record for them. Retaining the complete
            // sample here makes every extension reconstructible.
            raw: sample.raw.clone(),
        },
        evidence_index,
    ))
}

fn canonical_questions(
    sample: &DecodedSample,
    evidence_index: &HashMap<String, Vec<TurnLocation>>,
) -> anyhow::Result<Vec<QuestionRecord>> {
    sample
        .qa
        .iter()
        .enumerate()
        .map(|(question_index, raw_question)| {
            let question: LocomoQa =
                serde_json::from_value(raw_question.clone()).with_context(|| {
                    format!(
                        "decode LoCoMo sample {} question {question_index}",
                        sample.sample_id
                    )
                })?;
            let mut evidence = Vec::with_capacity(question.evidence.len());
            let mut gold_session_ids = Vec::new();
            let mut gold_turn_ids = Vec::new();

            for raw_evidence in question.evidence {
                let resolved = evidence_index
                    .get(&raw_evidence)
                    .and_then(|matches| (matches.len() == 1).then(|| &matches[0]));
                if let Some(location) = resolved {
                    gold_session_ids.push(location.session_id.clone());
                    gold_turn_ids.push(location.turn_id.clone());
                }
                evidence.push(EvidenceRef {
                    raw: raw_evidence,
                    resolved_turn_id: resolved.map(|location| location.turn_id.clone()),
                });
            }

            Ok(QuestionRecord {
                id: format!("{}#{question_index}", sample.sample_id),
                text: question.question,
                question_date: None,
                answer: question.answer,
                category: Some(question.category),
                question_type: None,
                evidence,
                gold_session_ids,
                gold_turn_ids,
                raw: raw_question.clone(),
            })
        })
        .collect()
}

fn session_number(key: &str) -> Option<usize> {
    key.strip_prefix("session_")?.parse().ok()
}

fn session_date(
    conversation: &Map<String, Value>,
    session_key: &str,
    sample_id: &str,
) -> anyhow::Result<Option<String>> {
    let date_key = format!("{session_key}_date_time");
    match conversation.get(&date_key) {
        Some(Value::String(date)) => Ok(Some(date.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => bail!("LoCoMo sample {sample_id} field {date_key} must be a string or null"),
    }
}

/// Compatibility view for the legacy phase-1 retrieval harness.
///
/// Unlike [`load_locomo`], this intentionally retains the historical
/// retrieval-only row filter until Task 12 removes the legacy harness.
pub fn parse_locomo(json: &str, split: EvalSplit) -> anyhow::Result<Vec<MemoryTask>> {
    let samples: Vec<LegacyLocomoSample> = serde_json::from_str(json)?;
    let mut tasks = Vec::new();
    for sample in samples {
        for (index, qa) in sample.qa.into_iter().enumerate() {
            if qa.category == LOCOMO_ADVERSARIAL_CATEGORY {
                continue;
            }
            if qa.evidence.is_empty() {
                continue;
            }
            tasks.push(MemoryTask {
                id: format!("{}#{index}", sample.sample_id),
                question: qa.question,
                gold_ids: qa.evidence,
                gold_answer: super::stringify_answer(&qa.answer),
            });
        }
    }
    Ok(match split {
        EvalSplit::Official => tasks,
        EvalSplit::Graphify => graphify_slice(&tasks, LOCOMO_GRAPHIFY_N).to_vec(),
    })
}
