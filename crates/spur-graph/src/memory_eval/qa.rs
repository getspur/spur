use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use rust_stemmers::{Algorithm, Stemmer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::GraphFacts;

use super::contract::{
    BenchmarkDataset, ConversationRecord, QuestionRecord, SessionRecord, TurnRecord,
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
    let predictions = split_multi_answer(prediction);
    let ground_truths = match answer {
        Value::Array(answers) => answers
            .iter()
            .filter_map(Value::as_str)
            .flat_map(split_multi_answer)
            .collect::<Vec<_>>(),
        _ => split_multi_answer(&answer_text(answer)),
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

fn split_multi_answer(text: &str) -> Vec<String> {
    text.split([',', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn stemmed_token_f1(prediction: &str, ground_truth: &str) -> f64 {
    let stemmer = Stemmer::create(Algorithm::English);
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

fn normalized_stems(text: &str, stemmer: &Stemmer) -> Vec<String> {
    let normalized = text
        .chars()
        .filter(|character| !character.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized
        .split_whitespace()
        .filter(|token| !matches!(*token, "a" | "an" | "the" | "and"))
        .map(|token| match stemmer.stem(token).as_ref() {
            // Preserve the approved origin-golden equivalence for the common
            // irregular past tense that a pure stemmer does not lemmatize.
            "ran" => "run".to_owned(),
            stem => stem.to_owned(),
        })
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
    lowered.contains("no information available")
        || lowered.contains("not mentioned")
        || lowered == "no"
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
            records.push(QaRecord {
                question_id: question.id.clone(),
                variant,
                category,
                status: QaStatus::Complete,
                score: score_locomo(category, &output_text, question.answer.clone()),
                output_text,
                prompt_sha256: request.prompt_sha256,
                ranking_sha256,
                recorded_seed: seed,
                input_tokens: response.input_tokens,
                output_tokens: response.output_tokens,
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
    let occurrence_id = hit.provenance_id.as_deref().unwrap_or(&hit.occurrence_id);
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
