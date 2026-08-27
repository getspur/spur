use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context};
use async_trait::async_trait;
use nltk_porter::{Mode, PorterStemmer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::GraphFacts;

use super::artifacts::ArtifactDigest;
use super::contract::{
    BenchmarkDataset, ConversationRecord, QuestionRecord, Role, SessionRecord, TurnRecord,
};
use super::ranking::{Granularity, QueryOccurrenceId, RankedHit, Ranking, RankingSet, Variant};
use super::retrieve::{facts_for_task, retrieve_task_hits, RetrievalReport};
use super::{coverage_milli, recall_at_k, MemoryTask, RECALL_K};

const LOCOMO_CONVERSATION_START: &str = "Below is a conversation between two people: {speaker_a} and {speaker_b}. The conversation takes place over multiple days and the date of each conversation is wriiten at the beginning of the conversation.\n\n";
const LOCOMO_QA_PROMPT: &str = "Based on the above context, write an answer in the form of a short phrase for the following question. Answer with exact words from the context whenever possible.\nQuestion: ";
const LOCOMO_QA_PROMPT_CATEGORY_5: &str =
    "Based on the above context, answer the following question.\n\nQuestion: ";
const LOCOMO_TEMPORAL_INSTRUCTION: &str =
    " Use DATE of CONVERSATION to answer with an approximate date.";
const LOCOMO_ABSTENTION_OPTION: &str = "Not mentioned in the conversation";
const LOCOMO_OPTION_ORDER_CONTRACT: &[u8] = b"locomo-adversarial-option-order-v1\0";

/// A backend-neutral, hash-bound LoCoMo reader request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QaRequest {
    pub question_id: String,
    pub variant: Variant,
    pub prompt: String,
    pub prompt_sha256: String,
    pub ranking_sha256: String,
    pub recorded_seed: u64,
}

/// Reader output and audited token usage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QaResponse {
    pub output_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Exact deterministic scorer input used after the LoCoMo reader returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocomoJudgeInput {
    pub category: u32,
    pub hypothesis: String,
    pub reference_answer: Value,
}

/// Exact deterministic scorer output that becomes the final LoCoMo label.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocomoJudgeOutput {
    pub score: f64,
}

/// Completion state shared with the resumable QA work in Task 10.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QaStatus {
    Complete,
    Pending,
}

/// One terminal LoCoMo QA result bound to the exact prompt and ranking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QaRecord {
    pub question_id: String,
    pub variant: Variant,
    pub category: u32,
    pub status: QaStatus,
    pub output_text: String,
    pub score: f64,
    pub prompt_sha256: String,
    pub ranking_sha256: String,
    pub recorded_seed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reader_request: QaRequest,
    pub reader_response: QaResponse,
    pub judge_input: LocomoJudgeInput,
    pub judge_output: LocomoJudgeOutput,
}

/// Reader seam; HTTP, judge, retry, and cache behavior belong to Task 10.
pub trait QaBackend {
    fn complete(&mut self, request: &QaRequest) -> anyhow::Result<QaResponse>;
}

/// Which displayed adversarial option is the correct abstention answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AdversarialChoice {
    A,
    B,
}

/// The deterministic two-option view expected by the released evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdversarialOptions {
    pub a: String,
    pub b: String,
    pub correct: AdversarialChoice,
}

impl AdversarialOptions {
    pub fn correct_text(&self) -> &str {
        match self.correct {
            AdversarialChoice::A => &self.a,
            AdversarialChoice::B => &self.b,
        }
    }

    pub fn false_text(&self) -> &str {
        match self.correct {
            AdversarialChoice::A => &self.b,
            AdversarialChoice::B => &self.a,
        }
    }

    fn resolve_output(&self, output: &str) -> String {
        match output.trim().to_ascii_lowercase().as_str() {
            "a" | "(a)" => self.a.clone(),
            "b" | "(b)" => self.b.clone(),
            _ => output.trim().to_owned(),
        }
    }
}

/// Explicit compatibility transform for the released category-5 schema.
///
/// The origin reader used `answer` as the plausible false option, while the
/// released rows store that value as `adversarial_answer` (and usually leave
/// `answer` absent). Keep that schema repair named and auditable.
pub fn released_adversarial_answer_compatibility_shim(
    question: &QuestionRecord,
) -> anyhow::Result<String> {
    ensure!(
        question.category == Some(5),
        "released adversarial-answer compatibility only applies to LoCoMo category 5"
    );
    question
        .raw
        .get("adversarial_answer")
        .and_then(Value::as_str)
        .filter(|answer| !answer.trim().is_empty())
        .or_else(|| {
            question
                .answer
                .as_str()
                .filter(|answer| !answer.trim().is_empty())
        })
        .map(str::to_owned)
        .context("LoCoMo category-5 row has no released adversarial_answer false option")
}

/// Derive replayable option order from the recorded seed and question identity.
pub fn locomo_adversarial_options(
    question: &QuestionRecord,
    seed: u64,
) -> anyhow::Result<AdversarialOptions> {
    let false_option = released_adversarial_answer_compatibility_shim(question)?;
    let mut hasher = Sha256::new();
    hasher.update(LOCOMO_OPTION_ORDER_CONTRACT);
    hasher.update(question.id.as_bytes());
    let question_offset = u64::from(hasher.finalize()[0]);
    let correct = if (seed ^ question_offset) & 1 == 0 {
        AdversarialChoice::A
    } else {
        AdversarialChoice::B
    };
    let (a, b) = match correct {
        AdversarialChoice::A => (LOCOMO_ABSTENTION_OPTION.to_owned(), false_option),
        AdversarialChoice::B => (false_option, LOCOMO_ABSTENTION_OPTION.to_owned()),
    };
    Ok(AdversarialOptions { a, b, correct })
}

/// Render the origin prompt with the recorded seed fixed to zero.
pub fn render_locomo_prompt(
    question: &QuestionRecord,
    ranking: &Ranking,
    dataset: &BenchmarkDataset,
) -> anyhow::Result<String> {
    render_locomo_prompt_with_seed(question, ranking, dataset, 0)
}

/// Render the origin LoCoMo reader prompt without changing ranking order.
pub fn render_locomo_prompt_with_seed(
    question: &QuestionRecord,
    ranking: &Ranking,
    dataset: &BenchmarkDataset,
    seed: u64,
) -> anyhow::Result<String> {
    ensure!(
        ranking.granularity == Granularity::Turn,
        "LoCoMo QA requires a frozen turn ranking"
    );
    ensure!(
        ranking.hits.len() <= ranking.k,
        "frozen ranking contains more hits than its declared k"
    );
    let category = question
        .category
        .context("LoCoMo QA question has no category")?;
    ensure!(
        (1..=5).contains(&category),
        "unsupported LoCoMo category {category}"
    );

    let conversation = question_conversation(dataset, question)?;
    let (speaker_a, speaker_b) = conversation_speakers(conversation)?;
    let mut prompt = LOCOMO_CONVERSATION_START
        .replace("{speaker_a}", speaker_a)
        .replace("{speaker_b}", speaker_b);

    for hit in &ranking.hits {
        let (session, turn) = ranked_turn(dataset, hit)?;
        let date = session
            .occurred_at
            .as_deref()
            .context("ranked LoCoMo turn has no session date")?;
        let speaker = turn
            .speaker
            .as_deref()
            .context("ranked LoCoMo turn has no speaker")?;
        prompt.push_str("DATE: ");
        prompt.push_str(date);
        prompt.push_str("\nCONVERSATION:\n");
        prompt.push_str(speaker);
        prompt.push_str(" said, \"");
        prompt.push_str(&turn.content);
        prompt.push_str("\"\n");
        if let Some(caption) = turn.caption.as_deref() {
            prompt.push_str(" and shared ");
            prompt.push_str(caption);
            prompt.push_str(".\n");
        }
        prompt.push('\n');
    }

    let mut rendered_question = question.text.clone();
    if category == 2 {
        rendered_question.push_str(LOCOMO_TEMPORAL_INSTRUCTION);
    } else if category == 5 {
        let options = locomo_adversarial_options(question, seed)?;
        rendered_question.push_str(" Select the correct answer: (a) ");
        rendered_question.push_str(&options.a);
        rendered_question.push_str(" (b) ");
        rendered_question.push_str(&options.b);
        rendered_question.push_str(". ");
    }

    prompt.push_str(if category == 5 {
        LOCOMO_QA_PROMPT_CATEGORY_5
    } else {
        LOCOMO_QA_PROMPT
    });
    prompt.push_str(&rendered_question);
    prompt.push_str(" Short answer:");
    Ok(prompt)
}

/// Score one answer with the category-specific origin contract.
pub fn score_locomo(category: u32, prediction: &str, answer: Value) -> f64 {
    match category {
        1 => multi_answer_f1(prediction, &answer),
        2 | 4 => stemmed_token_f1(prediction, &answer_text(&answer)),
        3 => stemmed_token_f1(
            prediction,
            answer_text(&answer)
                .split(';')
                .next()
                .unwrap_or_default()
                .trim(),
        ),
        5 => f64::from(is_adversarial_abstention(prediction)),
        _ => 0.0,
    }
}

fn multi_answer_f1(prediction: &str, answer: &Value) -> f64 {
    let (predictions, ground_truths) = match answer {
        // Canonical LoCoMo source rows store category-1 answers as one
        // comma-delimited string. Retain array support only as an explicit
        // compatibility path for callers using the earlier non-source shape.
        Value::Array(answers) => (
            split_multi_answer_array_compatibility(prediction),
            answers
                .iter()
                .filter_map(Value::as_str)
                .flat_map(split_multi_answer_array_compatibility)
                .collect::<Vec<_>>(),
        ),
        _ => (
            split_source_multi_answer(prediction),
            split_source_multi_answer(&answer_text(answer)),
        ),
    };
    if predictions.is_empty() || ground_truths.is_empty() {
        return 0.0;
    }
    ground_truths
        .iter()
        .map(|ground_truth| {
            predictions
                .iter()
                .map(|prediction| stemmed_token_f1(prediction, ground_truth))
                .fold(0.0, f64::max)
        })
        .sum::<f64>()
        / ground_truths.len() as f64
}

fn split_source_multi_answer(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).map(str::to_owned).collect()
}

fn split_multi_answer_array_compatibility(text: &str) -> Vec<String> {
    text.split([',', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn stemmed_token_f1(prediction: &str, ground_truth: &str) -> f64 {
    let stemmer = PorterStemmer::new(Mode::Nltk);
    let prediction_tokens = normalized_stems(prediction, &stemmer);
    let ground_truth_tokens = normalized_stems(ground_truth, &stemmer);
    let mut remaining = HashMap::<String, usize>::new();
    for token in &ground_truth_tokens {
        *remaining.entry(token.clone()).or_default() += 1;
    }
    let mut common = 0usize;
    for token in &prediction_tokens {
        if let Some(count) = remaining.get_mut(token).filter(|count| **count > 0) {
            *count -= 1;
            common += 1;
        }
    }
    if common == 0 || prediction_tokens.is_empty() || ground_truth_tokens.is_empty() {
        return 0.0;
    }
    let precision = common as f64 / prediction_tokens.len() as f64;
    let recall = common as f64 / ground_truth_tokens.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

fn normalized_stems(text: &str, stemmer: &PorterStemmer) -> Vec<String> {
    let normalized = text
        .chars()
        .filter(|character| !character.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized
        .split_whitespace()
        .filter(|token| !matches!(*token, "a" | "an" | "the" | "and"))
        .map(|token| stemmer.stem(token))
        .collect()
}

fn answer_text(answer: &Value) -> String {
    match answer {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(values) => values
            .iter()
            .map(answer_text)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null | Value::Object(_) => String::new(),
    }
}

fn is_adversarial_abstention(prediction: &str) -> bool {
    let lowered = prediction.trim().to_ascii_lowercase();
    lowered.contains("no information available") || lowered.contains("not mentioned")
}

/// Hash the exact serialized frozen ranking consumed by QA.
pub fn ranking_sha256(ranking: &Ranking) -> anyhow::Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(ranking)?))
}

/// Reject a QA record not bound to the ranking hash declared by its run.
pub fn validate_qa_ranking_hash(record: &QaRecord, expected: &str) -> anyhow::Result<()> {
    ensure!(
        record.ranking_sha256 == expected,
        "QA record ranking hash differs from frozen run ranking hash"
    );
    Ok(())
}

/// Evaluate every caller-keyed frozen LoCoMo turn ranking exactly once.
pub fn evaluate_locomo(
    dataset: &BenchmarkDataset,
    rankings: &RankingSet,
    backend: &mut dyn QaBackend,
    seed: u64,
) -> anyhow::Result<Vec<QaRecord>> {
    const VARIANTS: [Variant; 5] = [
        Variant::Oracle,
        Variant::Recent,
        Variant::FlatBm25,
        Variant::GraphIndexOnly,
        Variant::GraphTraversal,
    ];

    let mut records = Vec::new();
    for question in &dataset.questions {
        let category = question
            .category
            .context("LoCoMo QA question has no category")?;
        ensure!(
            (1..=5).contains(&category),
            "unsupported LoCoMo category {category}"
        );
        for variant in VARIANTS {
            let key = (
                QueryOccurrenceId::new(question.id.clone()),
                variant,
                Granularity::Turn,
            );
            let Some(ranking) = rankings.get(&key) else {
                continue;
            };
            ensure!(
                ranking.variant == variant && ranking.granularity == Granularity::Turn,
                "caller-owned ranking key disagrees with ranking payload"
            );
            ensure!(
                ranking.query_sha256 == sha256_hex(question.text.as_bytes()),
                "caller-owned question key is bound to a ranking with a different query hash"
            );

            let prompt = render_locomo_prompt_with_seed(question, ranking, dataset, seed)?;
            let ranking_sha256 = ranking_sha256(ranking)?;
            let request = QaRequest {
                question_id: question.id.clone(),
                variant,
                prompt_sha256: sha256_hex(prompt.as_bytes()),
                prompt,
                ranking_sha256: ranking_sha256.clone(),
                recorded_seed: seed,
            };
            let response = backend.complete(&request)?;
            let output_text = if category == 5 {
                locomo_adversarial_options(question, seed)?.resolve_output(&response.output_text)
            } else {
                response.output_text.trim().to_owned()
            };
            let judge_input = LocomoJudgeInput {
                category,
                hypothesis: output_text.clone(),
                reference_answer: question.answer.clone(),
            };
            let judge_output = LocomoJudgeOutput {
                score: score_locomo(
                    judge_input.category,
                    &judge_input.hypothesis,
                    judge_input.reference_answer.clone(),
                ),
            };
            records.push(QaRecord {
                question_id: question.id.clone(),
                variant,
                category,
                status: QaStatus::Complete,
                score: judge_output.score,
                output_text: output_text.clone(),
                prompt_sha256: request.prompt_sha256.clone(),
                ranking_sha256,
                recorded_seed: seed,
                input_tokens: response.input_tokens,
                output_tokens: response.output_tokens,
                reader_request: request,
                reader_response: response,
                judge_input,
                judge_output,
            });
        }
    }

    let frozen_turn_rankings = rankings
        .values()
        .filter(|ranking| ranking.granularity == Granularity::Turn)
        .count();
    ensure!(
        records.len() == frozen_turn_rankings,
        "frozen LoCoMo ranking set contains a caller key not present in the dataset"
    );
    Ok(records)
}

fn question_conversation<'a>(
    dataset: &'a BenchmarkDataset,
    question: &QuestionRecord,
) -> anyhow::Result<&'a ConversationRecord> {
    let source_id = question
        .id
        .rsplit_once('#')
        .map(|(source_id, _)| source_id)
        .unwrap_or(&question.id);
    dataset
        .conversations
        .iter()
        .find(|conversation| conversation.source_id.as_deref() == Some(source_id))
        .or_else(|| (dataset.conversations.len() == 1).then(|| &dataset.conversations[0]))
        .with_context(|| {
            format!(
                "no canonical conversation for LoCoMo question {}",
                question.id
            )
        })
}

fn conversation_speakers(conversation: &ConversationRecord) -> anyhow::Result<(&str, &str)> {
    if let Some(raw) = conversation.raw.get("conversation") {
        if let (Some(a), Some(b)) = (
            raw.get("speaker_a").and_then(Value::as_str),
            raw.get("speaker_b").and_then(Value::as_str),
        ) {
            return Ok((a, b));
        }
    }
    let mut speakers = conversation
        .sessions
        .iter()
        .flat_map(|session| session.turns.iter())
        .filter_map(|turn| turn.speaker.as_deref());
    let first = speakers
        .next()
        .context("LoCoMo conversation has no first speaker")?;
    let second = speakers
        .find(|speaker| *speaker != first)
        .context("LoCoMo conversation has no second speaker")?;
    Ok((first, second))
}

fn ranked_turn<'a>(
    dataset: &'a BenchmarkDataset,
    hit: &RankedHit,
) -> anyhow::Result<(&'a SessionRecord, &'a TurnRecord)> {
    let occurrence_id = hit.occurrence_id.as_str();
    dataset
        .all_sessions()
        .find_map(|session| {
            session
                .turns
                .iter()
                .find(|turn| turn.internal_id == occurrence_id)
                .map(|turn| (session, turn))
        })
        .with_context(|| format!("frozen ranking references unknown turn {occurrence_id}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Audited LongMemEval reader and judge snapshot.
pub const LONGMEMEVAL_MODEL: &str = "gpt-4o-2024-08-06";
/// Pinned standard input price for the audited LongMemEval model snapshot.
pub const LONGMEMEVAL_INPUT_USD_MICROS_PER_MILLION: u64 = 2_500_000;
/// Pinned standard output price for the audited LongMemEval model snapshot.
pub const LONGMEMEVAL_OUTPUT_USD_MICROS_PER_MILLION: u64 = 10_000_000;
/// Conservative ceiling for all billed Responses input, including provider
/// framing, under the pinned model's documented 128,000-token context window.
/// <https://developers.openai.com/api/docs/models/gpt-4o>
pub const LONGMEMEVAL_MAX_INPUT_TOKENS: u64 = 128_000;
/// Official Responses API create endpoint.
pub const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";

const OPENAI_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
const OPENAI_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OPENAI_READ_TIMEOUT: Duration = Duration::from_secs(60);
const OPENAI_PHYSICAL_TRANSMISSIONS_PER_CALL: u64 = 1;

const LONGMEM_READER_MAX_OUTPUT_TOKENS: u64 = 800;
const LONGMEM_JUDGE_MAX_OUTPUT_TOKENS: u64 = 10;
const LONGMEM_READER_PROMPT: &str = "I will give you several history chats between you and a user. Please answer the question based on the relevant chat history. Answer the question step by step: first extract all the relevant information, and then reason over the information to get the answer.\n\n\nHistory Chats:\n\n{history}\n\nCurrent Date: {question_date}\nQuestion: {question}\nAnswer (step by step):";

/// Which origin-native LongMemEval call produced a cache artifact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QaStage {
    Reader,
    Judge,
}

/// Audited Responses API token accounting.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct QaUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl QaUsage {
    fn validate(self) -> anyhow::Result<Self> {
        ensure!(
            self.input_tokens.checked_add(self.output_tokens) == Some(self.total_tokens),
            "malformed usage: total_tokens does not equal input_tokens + output_tokens"
        );
        Ok(self)
    }

    fn checked_add(self, other: Self) -> anyhow::Result<Self> {
        Ok(Self {
            input_tokens: self
                .input_tokens
                .checked_add(other.input_tokens)
                .context("input token usage overflow")?,
            output_tokens: self
                .output_tokens
                .checked_add(other.output_tokens)
                .context("output token usage overflow")?,
            total_tokens: self
                .total_tokens
                .checked_add(other.total_tokens)
                .context("total token usage overflow")?,
        })
    }
}

/// One pinned reader or judge request bound to immutable retrieval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LongMemQaRequest {
    pub question_id: String,
    pub variant: Variant,
    pub granularity: Granularity,
    pub stage: QaStage,
    pub model: String,
    pub prompt: String,
    pub question_sha256: String,
    pub prompt_sha256: String,
    pub ranking_sha256: String,
    pub max_output_tokens: u64,
}

/// One completed backend response, including the complete provider payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LongMemQaResponse {
    pub output_text: String,
    pub usage: QaUsage,
    pub raw_response: Value,
}

impl LongMemQaResponse {
    fn validate(mut self) -> anyhow::Result<Self> {
        ensure!(
            !self.output_text.trim().is_empty(),
            "completed response has empty output_text"
        );
        self.output_text = self.output_text.trim().to_owned();
        self.usage = self.usage.validate()?;
        let (decoded_output, decoded_usage) = decode_raw_response_fields(&self.raw_response)?;
        ensure!(
            self.output_text == decoded_output,
            "decoded output_text does not match raw_response"
        );
        ensure!(
            self.usage == decoded_usage,
            "decoded usage does not match raw_response"
        );
        Ok(self)
    }
}

fn decode_raw_response_fields(raw_response: &Value) -> anyhow::Result<(String, QaUsage)> {
    ensure!(
        raw_response.get("status").and_then(Value::as_str) == Some("completed"),
        "OpenAI response status is not completed"
    );
    let output_text = raw_response
        .get("output_text")
        .and_then(Value::as_str)
        .context("OpenAI response has no string output_text")?
        .trim()
        .to_owned();
    ensure!(
        !output_text.is_empty(),
        "completed response has empty output_text"
    );
    let usage = raw_response
        .get("usage")
        .and_then(Value::as_object)
        .context("OpenAI response has malformed usage")?;
    let usage = QaUsage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .context("OpenAI usage.input_tokens is malformed")?,
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .context("OpenAI usage.output_tokens is malformed")?,
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .context("OpenAI usage.total_tokens is malformed")?,
    }
    .validate()?;
    Ok((output_text, usage))
}

/// Complete cache identity for reader and judge artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct QaCacheKey {
    pub question_id: String,
    pub question_sha256: String,
    pub prompt_sha256: String,
    pub model: String,
    pub model_sha256: String,
    pub ranking_sha256: String,
    pub max_output_tokens: u64,
    pub variant: Variant,
    pub granularity: Granularity,
    pub stage: QaStage,
}

impl QaCacheKey {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        question_id: impl Into<String>,
        question_sha256: impl Into<String>,
        prompt_sha256: impl Into<String>,
        model: impl Into<String>,
        ranking_sha256: impl Into<String>,
        max_output_tokens: u64,
        variant: Variant,
        granularity: Granularity,
        stage: QaStage,
    ) -> Self {
        let model = model.into();
        Self {
            question_id: question_id.into(),
            question_sha256: question_sha256.into(),
            prompt_sha256: prompt_sha256.into(),
            model_sha256: sha256_hex(model.as_bytes()),
            model,
            ranking_sha256: ranking_sha256.into(),
            max_output_tokens,
            variant,
            granularity,
            stage,
        }
    }

    pub fn from_request(request: &LongMemQaRequest) -> Self {
        Self::new(
            request.question_id.clone(),
            request.question_sha256.clone(),
            request.prompt_sha256.clone(),
            request.model.clone(),
            request.ranking_sha256.clone(),
            request.max_output_tokens,
            request.variant,
            request.granularity,
            request.stage,
        )
    }

    /// File-safe digest over every cache-key field.
    pub fn identity_sha256(&self) -> String {
        let mut hasher = Sha256::new();
        let max_output_tokens = self.max_output_tokens.to_string();
        for component in [
            self.question_id.as_str(),
            self.question_sha256.as_str(),
            self.prompt_sha256.as_str(),
            self.model.as_str(),
            self.model_sha256.as_str(),
            self.ranking_sha256.as_str(),
            max_output_tokens.as_str(),
            variant_name(self.variant),
            granularity_name(self.granularity),
            stage_name(self.stage),
        ] {
            hasher.update((component.len() as u64).to_be_bytes());
            hasher.update(component.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }
}

/// Complete successful cache artifact. Failed calls are deliberately not cached.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QaCacheEntry {
    pub key: QaCacheKey,
    pub request: LongMemQaRequest,
    pub response: LongMemQaResponse,
    pub label: Option<bool>,
    pub cost_usd_micros: u64,
}

/// Cache seam used by the resumable evaluator.
pub trait QaCache {
    fn get(&self, key: &QaCacheKey) -> anyhow::Result<Option<QaCacheEntry>>;
    fn put(&mut self, entry: &QaCacheEntry) -> anyhow::Result<()>;
}

/// JSON artifact cache whose filenames are complete cache-identity digests.
#[derive(Debug, Clone)]
pub struct JsonQaCache {
    root: PathBuf,
}

impl JsonQaCache {
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        fs::create_dir_all(root.as_ref())
            .with_context(|| format!("create LongMemEval QA cache {}", root.as_ref().display()))?;
        Ok(Self {
            root: root.as_ref().to_path_buf(),
        })
    }

    pub fn entry_count(&self) -> anyhow::Result<usize> {
        Ok(fs::read_dir(&self.root)
            .with_context(|| format!("read QA cache {}", self.root.display()))?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            })
            .count())
    }

    /// Validate every cache file and retain only records reachable from the
    /// current frozen LongMemEval workload. The resulting digests are safe to
    /// offer for checksum reconciliation after a crash.
    pub fn validated_artifacts_for_run(
        &self,
        artifact_root: &Path,
        dataset: &BenchmarkDataset,
        rankings: &RankingSet,
    ) -> anyhow::Result<Vec<ArtifactDigest>> {
        let mut entries = HashMap::<QaCacheKey, (QaCacheEntry, ArtifactDigest)>::new();
        for directory_entry in fs::read_dir(&self.root)
            .with_context(|| format!("read QA cache {}", self.root.display()))?
        {
            let directory_entry = directory_entry?;
            let path = directory_entry.path();
            ensure!(
                directory_entry.file_type()?.is_file()
                    && path.extension().and_then(|extension| extension.to_str()) == Some("json"),
                "unrecognized LongMemEval QA cache artifact {}",
                path.display()
            );
            let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let entry: QaCacheEntry = serde_json::from_slice(&bytes)
                .with_context(|| format!("decode LongMemEval QA cache {}", path.display()))?;
            validate_reconciliation_cache_entry(&entry)?;
            let expected_name = format!("{}.json", entry.key.identity_sha256());
            ensure!(
                path.file_name().and_then(|name| name.to_str()) == Some(expected_name.as_str()),
                "LongMemEval QA cache filename does not match its complete identity"
            );
            let relative = path
                .strip_prefix(artifact_root)
                .with_context(|| format!("cache path {} is outside artifact root", path.display()))?
                .to_path_buf();
            let digest = ArtifactDigest {
                relative_path: relative,
                sha256: sha256_hex(&bytes),
            };
            ensure!(
                entries.insert(entry.key.clone(), (entry, digest)).is_none(),
                "duplicate LongMemEval QA cache identity"
            );
        }

        let mut recognized = HashSet::new();
        for ((question_id, variant, granularity), ranking) in rankings {
            let question = dataset
                .questions
                .iter()
                .find(|question| QueryOccurrenceId::new(question.id.clone()) == *question_id)
                .with_context(|| {
                    format!("ranking has unknown LongMemEval question {question_id:?}")
                })?;
            ensure!(
                ranking.variant == *variant && ranking.granularity == *granularity,
                "caller-owned ranking key disagrees with ranking payload"
            );
            let Ok(reader_request) = build_longmem_reader_request(question, ranking, dataset)
            else {
                continue;
            };
            let reader_key = QaCacheKey::from_request(&reader_request);
            let Some((reader_entry, _)) = entries.get(&reader_key) else {
                continue;
            };
            ensure!(
                reader_entry.request == reader_request,
                "reader cache request does not match the exact frozen request"
            );
            recognized.insert(reader_key);
            let Ok(judge_request) = build_longmem_judge_request(
                question,
                &reader_request,
                &reader_entry.response.output_text,
            ) else {
                continue;
            };
            let judge_key = QaCacheKey::from_request(&judge_request);
            let Some((judge_entry, _)) = entries.get(&judge_key) else {
                continue;
            };
            ensure!(
                judge_entry.request == judge_request,
                "judge cache request does not match the exact frozen request"
            );
            recognized.insert(judge_key);
        }
        ensure!(
            recognized.len() == entries.len(),
            "unrecognized LongMemEval QA cache record is outside the frozen workload"
        );

        let mut artifacts = entries
            .into_values()
            .map(|(_, artifact)| artifact)
            .collect::<Vec<_>>();
        artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(artifacts)
    }

    fn entry_path(&self, key: &QaCacheKey) -> PathBuf {
        self.root.join(format!("{}.json", key.identity_sha256()))
    }
}

impl QaCache for JsonQaCache {
    fn get(&self, key: &QaCacheKey) -> anyhow::Result<Option<QaCacheEntry>> {
        let path = self.entry_path(key);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let entry: QaCacheEntry =
            serde_json::from_slice(&bytes).with_context(|| format!("decode {}", path.display()))?;
        validate_cache_entry(&entry)?;
        ensure!(
            entry.key == *key,
            "cache artifact identity does not match its requested key"
        );
        Ok(Some(entry))
    }

    fn put(&mut self, entry: &QaCacheEntry) -> anyhow::Result<()> {
        validate_cache_entry(entry)?;
        let path = self.entry_path(&entry.key);
        let temporary = self
            .root
            .join(format!(".{}.tmp", entry.key.identity_sha256()));
        fs::write(&temporary, serde_json::to_vec_pretty(entry)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        fs::rename(&temporary, &path).with_context(|| format!("publish {}", path.display()))?;
        Ok(())
    }
}

fn validate_cache_entry(entry: &QaCacheEntry) -> anyhow::Result<()> {
    ensure!(
        entry.request.prompt_sha256 == sha256_hex(entry.request.prompt.as_bytes()),
        "cache request prompt_sha256 does not match its prompt bytes"
    );
    ensure!(
        entry.key == QaCacheKey::from_request(&entry.request),
        "cache entry key does not match its complete request identity"
    );
    entry.response.clone().validate()?;
    match entry.request.stage {
        QaStage::Reader => ensure!(
            entry.label.is_none(),
            "reader cache artifact has a judge label"
        ),
        QaStage::Judge => ensure!(
            entry.label == Some(judge_label(&entry.response.output_text)),
            "judge label does not match deterministic judge output"
        ),
    }
    Ok(())
}

fn validate_reconciliation_cache_entry(entry: &QaCacheEntry) -> anyhow::Result<()> {
    validate_cache_entry(entry)?;
    ensure!(
        entry.request.model == LONGMEMEVAL_MODEL,
        "cache entry model does not match the pinned LongMemEval model"
    );
    let expected_cost = token_cost_usd_micros(
        entry.response.usage,
        LONGMEMEVAL_INPUT_USD_MICROS_PER_MILLION,
        LONGMEMEVAL_OUTPUT_USD_MICROS_PER_MILLION,
    )?;
    ensure!(
        entry.cost_usd_micros == expected_cost,
        "cache entry cost does not match the pinned price of response usage"
    );
    Ok(())
}

/// Caller-declared paid-run ceilings and pre-request reservations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QaBudgetLimits {
    pub max_requests: u64,
    pub max_total_tokens: u64,
    pub max_usd_micros: u64,
    pub reserve_tokens_per_request: u64,
    pub reserve_usd_micros_per_request: u64,
    pub input_usd_micros_per_million: u64,
    pub output_usd_micros_per_million: u64,
}

/// Mutable accounting state checked before every non-cached request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QaBudget {
    limits: QaBudgetLimits,
    requests: u64,
    usage: QaUsage,
    cost_usd_micros: u64,
}

impl QaBudget {
    pub fn new(limits: QaBudgetLimits) -> Self {
        Self {
            limits,
            requests: 0,
            usage: QaUsage::default(),
            cost_usd_micros: 0,
        }
    }

    pub fn limits(&self) -> &QaBudgetLimits {
        &self.limits
    }

    pub fn requests(&self) -> u64 {
        self.requests
    }

    pub fn usage(&self) -> QaUsage {
        self.usage
    }

    pub fn cost_usd_micros(&self) -> u64 {
        self.cost_usd_micros
    }

    fn admit_request(&mut self, request: &LongMemQaRequest) -> anyhow::Result<()> {
        ensure!(
            request.model == LONGMEMEVAL_MODEL,
            "unsupported LongMemEval model for bounded QA admission"
        );
        let requests = self
            .requests
            .checked_add(1)
            .context("request count overflow")?;
        ensure!(
            requests <= self.limits.max_requests,
            "QA max request budget exhausted"
        );
        let maximum_total_tokens = LONGMEMEVAL_MAX_INPUT_TOKENS
            .checked_add(request.max_output_tokens)
            .context("QA maximum call token count overflow")?;
        let reserved_tokens = maximum_total_tokens.max(self.limits.reserve_tokens_per_request);
        ensure!(
            self.usage
                .total_tokens
                .checked_add(reserved_tokens)
                .is_some_and(|total| total <= self.limits.max_total_tokens),
            "QA token budget exhausted"
        );
        let maximum_call_cost = self
            .price_usage(QaUsage {
                input_tokens: LONGMEMEVAL_MAX_INPUT_TOKENS,
                output_tokens: request.max_output_tokens,
                total_tokens: maximum_total_tokens,
            })?
            .max(self.limits.reserve_usd_micros_per_request);
        ensure!(
            self.cost_usd_micros
                .checked_add(maximum_call_cost)
                .is_some_and(|cost| cost <= self.limits.max_usd_micros),
            "QA USD budget exhausted"
        );
        self.requests = requests;
        Ok(())
    }

    fn price_usage(&self, usage: QaUsage) -> anyhow::Result<u64> {
        let usage = usage.validate()?;
        token_cost_usd_micros(
            usage,
            self.limits.input_usd_micros_per_million,
            self.limits.output_usd_micros_per_million,
        )
    }

    fn record_usage(&mut self, usage: QaUsage, call_cost: u64) -> anyhow::Result<()> {
        let usage = usage.validate()?;
        let next_usage = self.usage.checked_add(usage)?;
        let next_cost = self
            .cost_usd_micros
            .checked_add(call_cost)
            .context("QA USD accounting overflow")?;
        self.usage = next_usage;
        self.cost_usd_micros = next_cost;
        ensure!(
            self.usage.total_tokens <= self.limits.max_total_tokens,
            "QA response exceeded the declared token ceiling"
        );
        ensure!(
            self.cost_usd_micros <= self.limits.max_usd_micros,
            "QA response exceeded the declared USD ceiling"
        );
        Ok(())
    }

    fn restore_cached_call(&mut self, entry: &QaCacheEntry) -> anyhow::Result<()> {
        let requests = self
            .requests
            .checked_add(1)
            .context("cached request count overflow")?;
        let usage = self.usage.checked_add(entry.response.usage.validate()?)?;
        let cost = self
            .cost_usd_micros
            .checked_add(entry.cost_usd_micros)
            .context("cached QA USD accounting overflow")?;
        ensure!(
            requests <= self.limits.max_requests,
            "cached QA max request budget exhausted"
        );
        ensure!(
            usage.total_tokens <= self.limits.max_total_tokens,
            "cached QA token budget exhausted"
        );
        ensure!(
            cost <= self.limits.max_usd_micros,
            "cached QA USD budget exhausted"
        );
        self.requests = requests;
        self.usage = usage;
        self.cost_usd_micros = cost;
        Ok(())
    }
}

fn token_cost_usd_micros(usage: QaUsage, input_rate: u64, output_rate: u64) -> anyhow::Result<u64> {
    let numerator = u128::from(usage.input_tokens)
        .checked_mul(u128::from(input_rate))
        .and_then(|value| {
            u128::from(usage.output_tokens)
                .checked_mul(u128::from(output_rate))
                .and_then(|output| value.checked_add(output))
        })
        .context("QA cost multiplication overflow")?;
    let rounded_up = numerator
        .checked_add(999_999)
        .context("QA cost rounding overflow")?
        / 1_000_000;
    u64::try_from(rounded_up).context("QA cost exceeds u64")
}

/// Async seam for fake and paid LongMemEval reader/judge backends.
#[async_trait]
pub trait LongMemQaBackend: Send {
    async fn complete(&mut self, request: &LongMemQaRequest) -> anyhow::Result<LongMemQaResponse>;
}

/// Single-transmission OpenAI Responses API adapter. Budgeting is owned by the
/// evaluator, and reqwest's implicit retries and redirects are explicitly disabled.
#[derive(Clone)]
pub struct OpenAiResponsesBackend {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl fmt::Debug for OpenAiResponsesBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesBackend")
            .field("credentials_available", &self.credentials_available())
            .field(
                "physical_transmissions_per_call",
                &OPENAI_PHYSICAL_TRANSMISSIONS_PER_CALL,
            )
            .field("retry_policy", &"never")
            .field("redirect_policy", &"none")
            .field("total_timeout", &OPENAI_TOTAL_TIMEOUT)
            .field("connect_timeout", &OPENAI_CONNECT_TIMEOUT)
            .field("read_timeout", &OPENAI_READ_TIMEOUT)
            .finish_non_exhaustive()
    }
}

impl OpenAiResponsesBackend {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(OPENAI_TOTAL_TIMEOUT)
                .connect_timeout(OPENAI_CONNECT_TIMEOUT)
                .read_timeout(OPENAI_READ_TIMEOUT)
                .build()
                .expect("static OpenAI Responses HTTP policy must build"),
            api_key,
        }
    }

    pub fn from_env() -> Self {
        Self::new(std::env::var("OPENAI_API_KEY").ok())
    }

    pub fn credentials_available(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
    }

    pub fn request_json(request: &LongMemQaRequest) -> anyhow::Result<Value> {
        ensure!(
            request.model == LONGMEMEVAL_MODEL,
            "LongMemEval audited model pin changed"
        );
        ensure!(!request.prompt.is_empty(), "LongMemEval prompt is empty");
        Ok(json!({
            "model": request.model,
            "input": request.prompt,
            "store": false,
            "temperature": 0,
            "max_output_tokens": request.max_output_tokens,
        }))
    }

    pub fn decode_response(status_code: u16, body: &[u8]) -> anyhow::Result<LongMemQaResponse> {
        ensure!(
            status_code == 200,
            "OpenAI Responses API HTTP status {status_code}"
        );
        let raw_response: Value = serde_json::from_slice(body).context("decode OpenAI response")?;
        let (output_text, usage) = decode_raw_response_fields(&raw_response)?;
        LongMemQaResponse {
            output_text,
            usage,
            raw_response,
        }
        .validate()
    }
}

#[async_trait]
impl LongMemQaBackend for OpenAiResponsesBackend {
    async fn complete(&mut self, request: &LongMemQaRequest) -> anyhow::Result<LongMemQaResponse> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .context("missing OPENAI_API_KEY")?;
        let response = self
            .client
            .post(OPENAI_RESPONSES_URL)
            .bearer_auth(api_key)
            .json(&Self::request_json(request)?)
            .send()
            .await
            .context("send OpenAI Responses API request")?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .context("read OpenAI Responses API response")?;
        Self::decode_response(status, &body)
    }
}

/// Render the origin extract-then-reason LongMemEval reader request.
pub fn build_longmem_reader_request(
    question: &QuestionRecord,
    ranking: &Ranking,
    dataset: &BenchmarkDataset,
) -> anyhow::Result<LongMemQaRequest> {
    ensure!(
        ranking.hits.len() <= ranking.k,
        "frozen ranking contains more hits than its declared k"
    );
    ensure!(
        ranking.query_sha256 == sha256_hex(question.text.as_bytes()),
        "caller-owned question key is bound to a ranking with a different query hash"
    );
    let conversation = dataset
        .conversations
        .iter()
        .find(|conversation| conversation.source_id.as_deref() == Some(question.id.as_str()))
        .with_context(|| format!("no canonical LongMemEval conversation for {}", question.id))?;
    let mut chunks = ranking
        .hits
        .iter()
        .map(|hit| longmem_prompt_chunk(conversation, hit, ranking.granularity))
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(!chunks.is_empty(), "LongMemEval reader history is empty");
    chunks.sort_by(|left, right| left.0.cmp(&right.0));

    let mut history = String::new();
    for (index, (date, turns)) in chunks.into_iter().enumerate() {
        history.push_str("\n### Session ");
        history.push_str(&(index + 1).to_string());
        history.push_str(":\nSession Date: ");
        history.push_str(&date);
        history.push_str("\nSession Content:\n\n");
        history.push_str(&serde_json::to_string(&turns)?);
        history.push('\n');
    }

    let question_date = question
        .question_date
        .as_deref()
        .context("LongMemEval question has no question date")?;
    let prompt = LONGMEM_READER_PROMPT
        .replace("{history}", &history)
        .replace("{question_date}", question_date)
        .replace("{question}", &question.text);
    Ok(LongMemQaRequest {
        question_id: question.id.clone(),
        variant: ranking.variant,
        granularity: ranking.granularity,
        stage: QaStage::Reader,
        model: LONGMEMEVAL_MODEL.to_owned(),
        question_sha256: sha256_hex(question.text.as_bytes()),
        prompt_sha256: sha256_hex(prompt.as_bytes()),
        ranking_sha256: ranking_sha256(ranking)?,
        max_output_tokens: LONGMEM_READER_MAX_OUTPUT_TOKENS,
        prompt,
    })
}

fn longmem_prompt_chunk(
    conversation: &ConversationRecord,
    hit: &RankedHit,
    granularity: Granularity,
) -> anyhow::Result<(String, Vec<Value>)> {
    match granularity {
        Granularity::Session => {
            let session = conversation
                .sessions
                .iter()
                .find(|session| session.internal_id == hit.occurrence_id)
                .with_context(|| {
                    format!(
                        "frozen LongMemEval ranking references unknown session {}",
                        hit.occurrence_id
                    )
                })?;
            Ok((
                longmem_session_date(session)?,
                session.turns.iter().map(longmem_turn_json).collect(),
            ))
        }
        Granularity::Turn => {
            for session in &conversation.sessions {
                if let Some(index) = session
                    .turns
                    .iter()
                    .position(|turn| turn.internal_id == hit.occurrence_id)
                {
                    let end = (index + 2).min(session.turns.len());
                    return Ok((
                        longmem_session_date(session)?,
                        session.turns[index..end]
                            .iter()
                            .map(longmem_turn_json)
                            .collect(),
                    ));
                }
            }
            anyhow::bail!(
                "frozen LongMemEval ranking references unknown turn {}",
                hit.occurrence_id
            )
        }
    }
}

fn longmem_session_date(session: &SessionRecord) -> anyhow::Result<String> {
    session
        .occurred_at
        .clone()
        .context("ranked LongMemEval session has no date")
}

fn longmem_turn_json(turn: &TurnRecord) -> Value {
    json!({
        "role": match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Other => "other",
        },
        "content": turn.content,
    })
}

/// Render the pinned origin LongMemEval answer-check prompt.
pub fn render_longmem_judge_prompt(
    question_type: &str,
    question: &str,
    answer: &Value,
    hypothesis: &str,
    abstention: bool,
) -> anyhow::Result<String> {
    let answer = longmem_answer_text(answer)?;
    let prompt = if abstention {
        format!(
            "I will give you an unanswerable question, an explanation, and a response from a model. Please answer yes if the model correctly identifies the question as unanswerable. The model could say that the information is incomplete, or some other information is given but the asked information is not.\n\nQuestion: {question}\n\nExplanation: {answer}\n\nModel Response: {hypothesis}\n\nDoes the model correctly identify the question as unanswerable? Answer yes or no only."
        )
    } else {
        match question_type {
            "single-session-user" | "single-session-assistant" | "multi-session" => format!(
                "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no. If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. If the response only contains a subset of the information required by the answer, answer no.\n\nQuestion: {question}\n\nCorrect Answer: {answer}\n\nModel Response: {hypothesis}\n\nIs the model response correct? Answer yes or no only."
            ),
            "temporal-reasoning" => format!(
                "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no. If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. If the response only contains a subset of the information required by the answer, answer no. In addition, do not penalize off-by-one errors for the number of days.\nIf the question asks for the number of days/weeks/months, etc., and the model makes off-by-one errors (e.g., predicting 19 days when the answer is 18), the model's response is still correct. \n\nQuestion: {question}\n\nCorrect Answer: {answer}\n\nModel Response: {hypothesis}\n\nIs the model response correct? Answer yes or no only."
            ),
            "knowledge-update" => format!(
                "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no. If the response contains some previous information along with an updated answer, the response should be considered as correct as long as the updated answer is the required answer.\n\nQuestion: {question}\n\nCorrect Answer: {answer}\n\nModel Response: {hypothesis}\n\nIs the model response correct? Answer yes or no only."
            ),
            "single-session-preference" => format!(
                "I will give you a question, a rubric for desired personalized response, and a response from a model. Please answer yes if the response satisfies the desired response. Otherwise, answer no. The model does not need to reflect all the points in the rubric. The response is correct as long as it recalls and utilizes the user's personal information correctly.\n\nQuestion: {question}\n\nRubric: {answer}\n\nModel Response: {hypothesis}\n\nIs the model response correct? Answer yes or no only."
            ),
            _ => anyhow::bail!("unsupported LongMemEval question type {question_type}"),
        }
    };
    Ok(prompt)
}

fn longmem_answer_text(answer: &Value) -> anyhow::Result<String> {
    match answer {
        Value::String(text) => Ok(text.clone()),
        _ => serde_json::to_string(answer).context("serialize LongMemEval answer"),
    }
}

fn build_longmem_judge_request(
    question: &QuestionRecord,
    reader: &LongMemQaRequest,
    hypothesis: &str,
) -> anyhow::Result<LongMemQaRequest> {
    let question_type = question
        .question_type
        .as_deref()
        .context("LongMemEval question has no question type")?;
    let prompt = render_longmem_judge_prompt(
        question_type,
        &question.text,
        &question.answer,
        hypothesis,
        question.id.contains("_abs"),
    )?;
    Ok(LongMemQaRequest {
        question_id: question.id.clone(),
        variant: reader.variant,
        granularity: reader.granularity,
        stage: QaStage::Judge,
        model: LONGMEMEVAL_MODEL.to_owned(),
        question_sha256: sha256_hex(question.text.as_bytes()),
        prompt_sha256: sha256_hex(prompt.as_bytes()),
        ranking_sha256: reader.ranking_sha256.clone(),
        max_output_tokens: LONGMEM_JUDGE_MAX_OUTPUT_TOKENS,
        prompt,
    })
}

/// One denominator-preserving LongMemEval QA result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LongMemQaRecord {
    pub question_id: String,
    pub variant: Variant,
    pub granularity: Granularity,
    pub question_type: String,
    pub status: QaStatus,
    pub label: Option<bool>,
    pub hypothesis: Option<String>,
    pub pending_reason: Option<String>,
    pub model: String,
    pub question_sha256: String,
    pub ranking_sha256: String,
    pub reader_prompt_sha256: Option<String>,
    pub judge_prompt_sha256: Option<String>,
    pub usage: QaUsage,
    pub cost_usd_micros: u64,
    pub reader_request: Option<LongMemQaRequest>,
    pub reader_response: Option<LongMemQaResponse>,
    pub judge_request: Option<LongMemQaRequest>,
    pub judge_response: Option<LongMemQaResponse>,
}

#[derive(Debug, Clone, Default)]
struct LongMemQaAudit {
    reader_request: Option<LongMemQaRequest>,
    reader_response: Option<LongMemQaResponse>,
    judge_request: Option<LongMemQaRequest>,
    judge_response: Option<LongMemQaResponse>,
}

impl LongMemQaRecord {
    fn pending(
        question: &QuestionRecord,
        ranking: &Ranking,
        ranking_sha256: String,
        audit: LongMemQaAudit,
        usage: QaUsage,
        cost_usd_micros: u64,
        reason: impl Into<String>,
    ) -> Self {
        let reader_prompt_sha256 = audit
            .reader_request
            .as_ref()
            .map(|request| request.prompt_sha256.clone());
        let judge_prompt_sha256 = audit
            .judge_request
            .as_ref()
            .map(|request| request.prompt_sha256.clone());
        let hypothesis = audit
            .reader_response
            .as_ref()
            .map(|response| response.output_text.clone());
        Self {
            question_id: question.id.clone(),
            variant: ranking.variant,
            granularity: ranking.granularity,
            question_type: question.question_type.clone().unwrap_or_default(),
            status: QaStatus::Pending,
            label: None,
            hypothesis,
            pending_reason: Some(reason.into()),
            model: LONGMEMEVAL_MODEL.to_owned(),
            question_sha256: sha256_hex(question.text.as_bytes()),
            ranking_sha256,
            reader_prompt_sha256,
            judge_prompt_sha256,
            usage,
            cost_usd_micros,
            reader_request: audit.reader_request,
            reader_response: audit.reader_response,
            judge_request: audit.judge_request,
            judge_response: audit.judge_response,
        }
    }
}

/// Replay pinned reader then pinned judge over each supplied frozen ranking.
/// Operational failure is represented by one `QaPending` record, never by a
/// missing row or a fabricated label.
pub async fn evaluate_longmem(
    dataset: &BenchmarkDataset,
    rankings: &RankingSet,
    backend: &mut dyn LongMemQaBackend,
    cache: &mut dyn QaCache,
    budget: &mut QaBudget,
) -> anyhow::Result<Vec<LongMemQaRecord>> {
    let mut records = Vec::with_capacity(rankings.len());
    for ((question_id, variant, granularity), ranking) in rankings {
        let question = dataset
            .questions
            .iter()
            .find(|question| QueryOccurrenceId::new(question.id.clone()) == *question_id)
            .with_context(|| format!("ranking has unknown LongMemEval question {question_id:?}"))?;
        ensure!(
            ranking.variant == *variant && ranking.granularity == *granularity,
            "caller-owned ranking key disagrees with ranking payload"
        );
        let raw_ranking_sha256 = ranking_sha256(ranking)?;
        let reader_request = match build_longmem_reader_request(question, ranking, dataset) {
            Ok(request) => request,
            Err(error) => {
                records.push(LongMemQaRecord::pending(
                    question,
                    ranking,
                    raw_ranking_sha256,
                    LongMemQaAudit::default(),
                    QaUsage::default(),
                    0,
                    format!("reader request: {error:#}"),
                ));
                continue;
            }
        };
        let mut audit = LongMemQaAudit {
            reader_request: Some(reader_request.clone()),
            ..LongMemQaAudit::default()
        };
        let reader_key = QaCacheKey::from_request(&reader_request);
        let reader_entry = match cache.get(&reader_key) {
            Ok(Some(entry)) => {
                audit.reader_response = Some(entry.response.clone());
                if let Err(error) = budget.restore_cached_call(&entry) {
                    records.push(LongMemQaRecord::pending(
                        question,
                        ranking,
                        reader_request.ranking_sha256.clone(),
                        audit,
                        QaUsage::default(),
                        0,
                        format!("reader cache budget: {error:#}"),
                    ));
                    continue;
                }
                entry
            }
            Ok(None) => match execute_cached_call(
                backend,
                cache,
                budget,
                reader_key,
                reader_request.clone(),
                None,
            )
            .await
            {
                Ok(entry) => entry,
                Err(failure) => {
                    let reason = format!("{:#}", failure.error);
                    let (usage, cost_usd_micros) = if let Some(received) = failure.received {
                        let usage = received.response.usage;
                        let cost_usd_micros = received.cost_usd_micros.unwrap_or(0);
                        audit.reader_response = Some(received.response);
                        (usage, cost_usd_micros)
                    } else {
                        (QaUsage::default(), 0)
                    };
                    records.push(LongMemQaRecord::pending(
                        question,
                        ranking,
                        reader_request.ranking_sha256.clone(),
                        audit,
                        usage,
                        cost_usd_micros,
                        format!("reader: {reason}"),
                    ));
                    continue;
                }
            },
            Err(error) => {
                records.push(LongMemQaRecord::pending(
                    question,
                    ranking,
                    reader_request.ranking_sha256.clone(),
                    audit,
                    QaUsage::default(),
                    0,
                    format!("reader cache: {error:#}"),
                ));
                continue;
            }
        };
        audit.reader_response = Some(reader_entry.response.clone());
        if reader_entry.request.stage != QaStage::Reader || reader_entry.label.is_some() {
            records.push(LongMemQaRecord::pending(
                question,
                ranking,
                reader_request.ranking_sha256.clone(),
                audit,
                QaUsage::default(),
                0,
                "reader cache entry has judge-only fields",
            ));
            continue;
        }
        let hypothesis = reader_entry.response.output_text.clone();
        let judge_request =
            match build_longmem_judge_request(question, &reader_request, &hypothesis) {
                Ok(request) => request,
                Err(error) => {
                    records.push(LongMemQaRecord::pending(
                        question,
                        ranking,
                        reader_request.ranking_sha256.clone(),
                        audit,
                        reader_entry.response.usage,
                        reader_entry.cost_usd_micros,
                        format!("judge request: {error:#}"),
                    ));
                    continue;
                }
            };
        audit.judge_request = Some(judge_request.clone());
        let judge_key = QaCacheKey::from_request(&judge_request);
        let judge_entry = match cache.get(&judge_key) {
            Ok(Some(entry)) => {
                audit.judge_response = Some(entry.response.clone());
                if let Err(error) = budget.restore_cached_call(&entry) {
                    records.push(LongMemQaRecord::pending(
                        question,
                        ranking,
                        reader_request.ranking_sha256.clone(),
                        audit,
                        reader_entry.response.usage,
                        reader_entry.cost_usd_micros,
                        format!("judge cache budget: {error:#}"),
                    ));
                    continue;
                }
                entry
            }
            Ok(None) => match execute_cached_call(
                backend,
                cache,
                budget,
                judge_key,
                judge_request.clone(),
                Some(judge_label),
            )
            .await
            {
                Ok(entry) => entry,
                Err(failure) => {
                    let reason = format!("{:#}", failure.error);
                    let (usage, cost_usd_micros) = if let Some(received) = failure.received {
                        let usage = reader_entry
                            .response
                            .usage
                            .checked_add(received.response.usage)?;
                        let cost_usd_micros = reader_entry
                            .cost_usd_micros
                            .checked_add(received.cost_usd_micros.unwrap_or(0))
                            .context("pending QA cost overflow")?;
                        audit.judge_response = Some(received.response);
                        (usage, cost_usd_micros)
                    } else {
                        (reader_entry.response.usage, reader_entry.cost_usd_micros)
                    };
                    records.push(LongMemQaRecord::pending(
                        question,
                        ranking,
                        reader_request.ranking_sha256.clone(),
                        audit,
                        usage,
                        cost_usd_micros,
                        format!("judge: {reason}"),
                    ));
                    continue;
                }
            },
            Err(error) => {
                records.push(LongMemQaRecord::pending(
                    question,
                    ranking,
                    reader_request.ranking_sha256.clone(),
                    audit,
                    reader_entry.response.usage,
                    reader_entry.cost_usd_micros,
                    format!("judge cache: {error:#}"),
                ));
                continue;
            }
        };
        audit.judge_response = Some(judge_entry.response.clone());
        let Some(label) = judge_entry.label else {
            records.push(LongMemQaRecord::pending(
                question,
                ranking,
                reader_request.ranking_sha256.clone(),
                audit,
                reader_entry.response.usage,
                reader_entry.cost_usd_micros,
                "judge cache entry has no terminal label",
            ));
            continue;
        };
        records.push(LongMemQaRecord {
            question_id: question.id.clone(),
            variant: ranking.variant,
            granularity: ranking.granularity,
            question_type: question.question_type.clone().unwrap_or_default(),
            status: QaStatus::Complete,
            label: Some(label),
            hypothesis: Some(hypothesis),
            pending_reason: None,
            model: LONGMEMEVAL_MODEL.to_owned(),
            question_sha256: reader_request.question_sha256.clone(),
            ranking_sha256: reader_request.ranking_sha256.clone(),
            reader_prompt_sha256: Some(reader_request.prompt_sha256.clone()),
            judge_prompt_sha256: Some(judge_request.prompt_sha256.clone()),
            usage: reader_entry
                .response
                .usage
                .checked_add(judge_entry.response.usage)?,
            cost_usd_micros: reader_entry
                .cost_usd_micros
                .checked_add(judge_entry.cost_usd_micros)
                .context("cached QA cost overflow")?,
            reader_request: Some(reader_request),
            reader_response: Some(reader_entry.response),
            judge_request: Some(judge_request),
            judge_response: Some(judge_entry.response),
        });
    }
    ensure!(
        records.len() == rankings.len(),
        "LongMemEval QA did not retain its frozen-ranking denominator"
    );
    Ok(records)
}

async fn execute_cached_call(
    backend: &mut dyn LongMemQaBackend,
    cache: &mut dyn QaCache,
    budget: &mut QaBudget,
    key: QaCacheKey,
    request: LongMemQaRequest,
    labeler: Option<fn(&str) -> bool>,
) -> Result<QaCacheEntry, QaCallFailure> {
    budget
        .admit_request(&request)
        .map_err(QaCallFailure::before_response)?;
    let response = backend
        .complete(&request)
        .await
        .and_then(LongMemQaResponse::validate)
        .map_err(QaCallFailure::before_response)?;
    let cost_usd_micros = budget
        .price_usage(response.usage)
        .map_err(|error| QaCallFailure::after_response(error, response.clone(), None))?;
    if let Err(error) = budget.record_usage(response.usage, cost_usd_micros) {
        return Err(QaCallFailure::after_response(
            error,
            response,
            Some(cost_usd_micros),
        ));
    }
    let entry = QaCacheEntry {
        key,
        request,
        label: labeler.map(|labeler| labeler(&response.output_text)),
        response,
        cost_usd_micros,
    };
    if let Err(error) = cache.put(&entry) {
        return Err(QaCallFailure::after_response(
            error,
            entry.response,
            Some(cost_usd_micros),
        ));
    }
    Ok(entry)
}

#[derive(Debug)]
struct QaCallFailure {
    error: anyhow::Error,
    received: Option<QaReceivedCall>,
}

impl QaCallFailure {
    fn before_response(error: anyhow::Error) -> Self {
        Self {
            error,
            received: None,
        }
    }

    fn after_response(
        error: anyhow::Error,
        response: LongMemQaResponse,
        cost_usd_micros: Option<u64>,
    ) -> Self {
        Self {
            error,
            received: Some(QaReceivedCall {
                response,
                cost_usd_micros,
            }),
        }
    }
}

#[derive(Debug)]
struct QaReceivedCall {
    response: LongMemQaResponse,
    cost_usd_micros: Option<u64>,
}

fn judge_label(output: &str) -> bool {
    output.to_ascii_lowercase().contains("yes")
}

fn variant_name(variant: Variant) -> &'static str {
    match variant {
        Variant::Oracle => "oracle",
        Variant::Recent => "recent",
        Variant::FlatBm25 => "flat_bm25",
        Variant::GraphIndexOnly => "graph_index_only",
        Variant::GraphTraversal => "graph_traversal",
    }
}

fn granularity_name(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Session => "session",
        Granularity::Turn => "turn",
    }
}

fn stage_name(stage: QaStage) -> &'static str {
    match stage {
        QaStage::Reader => "reader",
        QaStage::Judge => "judge",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactVerdict {
    Covered,
    Partial,
    Miss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaReport {
    pub n: usize,
    pub k: usize,
    pub covered: u32,
    pub partial: u32,
    pub miss: u32,
    pub coverage_milli: u32,
}

pub fn grade_key_fact(hypothesis: &str, gold: &str) -> FactVerdict {
    let hypo = hypothesis.to_ascii_lowercase();
    let gold = gold.trim().to_ascii_lowercase();
    if gold.is_empty() {
        return FactVerdict::Miss;
    }
    if hypo.contains(&gold) {
        return FactVerdict::Covered;
    }
    let tokens: Vec<&str> = gold
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .collect();
    if tokens.iter().any(|token| hypo.contains(token)) {
        return FactVerdict::Partial;
    }
    FactVerdict::Miss
}

pub fn extractive_qa(root: &Path, tasks: &[MemoryTask]) -> anyhow::Result<QaReport> {
    let mut cache: HashMap<PathBuf, GraphFacts> = HashMap::new();
    let mut covered = 0u32;
    let mut partial = 0u32;
    let mut miss = 0u32;
    for task in tasks {
        let hits = retrieve_task_hits(facts_for_task(&mut cache, root, task)?, &task.question);
        let context_hits = hits.len().min(RECALL_K);
        debug_assert!(context_hits <= RECALL_K);
        let hypothesis: String = hits
            .iter()
            .take(RECALL_K)
            .map(|hit| hit.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        match grade_key_fact(&hypothesis, &task.gold_answer) {
            FactVerdict::Covered => covered += 1,
            FactVerdict::Partial => partial += 1,
            FactVerdict::Miss => miss += 1,
        }
    }
    let n = tasks.len();
    Ok(QaReport {
        n,
        k: RECALL_K,
        covered,
        partial,
        miss,
        coverage_milli: coverage_milli(covered, partial, n as u32),
    })
}

pub fn evaluate_tasks(
    root: &Path,
    tasks: &[MemoryTask],
) -> anyhow::Result<(RetrievalReport, QaReport)> {
    let mut cache: HashMap<PathBuf, GraphFacts> = HashMap::new();
    let mut total = 0u64;
    let mut covered = 0u32;
    let mut partial = 0u32;
    let mut miss = 0u32;
    for task in tasks {
        let hits = retrieve_task_hits(facts_for_task(&mut cache, root, task)?, &task.question);
        let ids: Vec<&str> = hits.iter().map(|hit| hit.id.as_str()).collect();
        total += u64::from(recall_at_k(&task.gold_ids, &ids, RECALL_K));
        let hypothesis: String = hits
            .iter()
            .take(RECALL_K)
            .map(|hit| hit.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        match grade_key_fact(&hypothesis, &task.gold_answer) {
            FactVerdict::Covered => covered += 1,
            FactVerdict::Partial => partial += 1,
            FactVerdict::Miss => miss += 1,
        }
    }
    let n = tasks.len();
    let mean_recall_milli = if n == 0 { 0 } else { (total / n as u64) as u32 };
    Ok((
        RetrievalReport {
            n,
            k: RECALL_K,
            mean_recall_milli,
        },
        QaReport {
            n,
            k: RECALL_K,
            covered,
            partial,
            miss,
            coverage_milli: coverage_milli(covered, partial, n as u32),
        },
    ))
}
