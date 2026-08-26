use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::memory_eval::RECALL_K;
use crate::{build_facts, GraphFacts, NodeKind};

use super::{recall_at_k, MemoryTask};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalReport {
    pub n: usize,
    pub k: usize,
    pub mean_recall_milli: u32,
}

pub fn retrieve_seed_expand(root: &Path, tasks: &[MemoryTask]) -> anyhow::Result<RetrievalReport> {
    if tasks.is_empty() {
        return Ok(RetrievalReport {
            n: 0,
            k: RECALL_K,
            mean_recall_milli: 0,
        });
    }
    let mut cache: HashMap<PathBuf, GraphFacts> = HashMap::new();
    let mut total = 0u64;
    for task in tasks {
        let hits = retrieve_task_hits(facts_for_task(&mut cache, root, task)?, &task.question);
        let ids: Vec<&str> = hits.iter().map(|hit| hit.id.as_str()).collect();
        total += u64::from(recall_at_k(&task.gold_ids, &ids, RECALL_K));
    }
    Ok(RetrievalReport {
        n: tasks.len(),
        k: RECALL_K,
        mean_recall_milli: (total / tasks.len() as u64) as u32,
    })
}

pub fn retrieve_task_ids(root: &Path, task: &MemoryTask) -> anyhow::Result<Vec<String>> {
    let mut cache: HashMap<PathBuf, GraphFacts> = HashMap::new();
    Ok(
        retrieve_task_hits(facts_for_task(&mut cache, root, task)?, &task.question)
            .into_iter()
            .map(|hit| hit.id)
            .collect(),
    )
}

pub(crate) fn haystack_root(root: &Path, task: &MemoryTask) -> PathBuf {
    let corpus = task.id.split('#').next().unwrap_or(task.id.as_str());
    let scoped = root.join(corpus);
    if scoped.is_dir() {
        scoped
    } else {
        root.to_path_buf()
    }
}

pub(crate) fn facts_for_task<'a>(
    cache: &'a mut HashMap<PathBuf, GraphFacts>,
    root: &Path,
    task: &MemoryTask,
) -> anyhow::Result<&'a GraphFacts> {
    let haystack = haystack_root(root, task);
    if !cache.contains_key(&haystack) {
        let facts = build_facts(&haystack, None)?.0;
        cache.insert(haystack.clone(), facts);
    }
    Ok(cache.get(&haystack).expect("haystack facts just inserted"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetrievalHit {
    pub id: String,
    pub text: String,
}

pub(crate) fn retrieve_task_hits(facts: &GraphFacts, question: &str) -> Vec<RetrievalHit> {
    // sol_07a8eb8af5064466: topk_ids, full_session_text, seed_k=hit_k=10.
    let terms = query_terms(question);
    let mut by_id: HashMap<String, (u32, Vec<String>)> = HashMap::new();
    for node in &facts.nodes {
        if node.kind != NodeKind::Section && node.kind != NodeKind::File {
            continue;
        }
        let turn = turn_id(&node.label);
        if turn.is_empty() {
            continue;
        }
        let score = term_score(&node.label, &terms);
        let entry = by_id.entry(turn).or_insert((0, Vec::new()));
        if score > entry.0 {
            entry.0 = score;
        }
        entry.1.push(node.label.clone());
    }
    let mut ranked: Vec<(u32, String, String)> = by_id
        .into_iter()
        .filter(|(_, (score, _))| *score > 0)
        .map(|(id, (score, texts))| (score, id, texts.join(" ")))
        .collect();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    ranked
        .into_iter()
        .take(RECALL_K)
        .map(|(_, id, text)| RetrievalHit { id, text })
        .collect()
}

fn query_terms(question: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "a", "an", "the", "what", "who", "when", "where", "did", "does", "do", "for", "of", "to",
        "and", "or", "in", "on", "is", "was", "i",
    ];
    question
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.to_ascii_lowercase())
        .filter(|token| token.len() > 1 && !STOP.contains(&token.as_str()))
        .collect()
}

fn term_score(label: &str, terms: &[String]) -> u32 {
    let lower = label.to_ascii_lowercase();
    terms
        .iter()
        .filter(|term| lower.contains(term.as_str()))
        .count() as u32
}

fn turn_id(label: &str) -> String {
    label
        .split_whitespace()
        .next()
        .filter(|token| is_session_or_turn_id(token))
        .unwrap_or_default()
        .to_string()
}

fn is_session_or_turn_id(token: &str) -> bool {
    if token.is_empty() || token.contains('/') || token.ends_with(".md") {
        return false;
    }
    token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':')
        && token.chars().any(|ch| ch.is_ascii_alphanumeric())
}
