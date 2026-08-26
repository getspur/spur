use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;

use crate::memory_eval::RECALL_K;
use crate::{build_facts, GraphFacts, NodeId, NodeKind};

use super::{recall_at_k, MemoryTask, EXPAND_DEPTH, HUB_DEGREE_FLOOR};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalReport {
    pub n: usize,
    pub k: usize,
    pub mean_recall_milli: u32,
}

pub fn retrieve_seed_expand(root: &Path, tasks: &[MemoryTask]) -> anyhow::Result<RetrievalReport> {
    let facts = build_facts(root, None)?.0;
    if tasks.is_empty() {
        return Ok(RetrievalReport {
            n: 0,
            k: RECALL_K,
            mean_recall_milli: 0,
        });
    }
    let mut total = 0u64;
    for task in tasks {
        let hits = retrieve_task_hits(&facts, &task.question);
        let ids: Vec<&str> = hits.iter().map(|hit| hit.id.as_str()).collect();
        total += u64::from(recall_at_k(&task.gold_ids, &ids, RECALL_K));
    }
    Ok(RetrievalReport {
        n: tasks.len(),
        k: RECALL_K,
        mean_recall_milli: (total / tasks.len() as u64) as u32,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetrievalHit {
    pub id: String,
    pub text: String,
}

pub(crate) fn retrieve_task_hits(facts: &GraphFacts, question: &str) -> Vec<RetrievalHit> {
    let terms = query_terms(question);
    let mut scored: Vec<(u32, NodeId, String, String)> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Section || node.kind == NodeKind::File)
        .map(|node| {
            let score = term_score(&node.label, &terms);
            (
                score,
                node.node_id,
                turn_id(&node.label),
                node.label.clone(),
            )
        })
        .filter(|(score, _, _, _)| *score > 0)
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.2.cmp(&right.2)));

    let seeds: Vec<NodeId> = scored.iter().take(3).map(|row| row.1).collect();
    let expanded = expand(facts, &seeds);
    let mut hits = Vec::new();
    let mut seen = BTreeSet::new();
    for (_, node_id, turn, text) in &scored {
        if !expanded.contains(node_id) {
            continue;
        }
        if turn.is_empty() || !seen.insert(turn.clone()) {
            continue;
        }
        hits.push(RetrievalHit {
            id: turn.clone(),
            text: text.clone(),
        });
        if hits.len() >= RECALL_K {
            break;
        }
    }
    if hits.is_empty() {
        for node in &facts.nodes {
            if !expanded.contains(&node.node_id) {
                continue;
            }
            let turn = turn_id(&node.label);
            if turn.is_empty() || !seen.insert(turn.clone()) {
                continue;
            }
            hits.push(RetrievalHit {
                id: turn,
                text: node.label.clone(),
            });
            if hits.len() >= RECALL_K {
                break;
            }
        }
    }
    hits
}

fn expand(facts: &GraphFacts, seeds: &[NodeId]) -> BTreeSet<NodeId> {
    let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for edge in &facts.edges {
        if let Some(target) = edge.target_node_id {
            adjacency
                .entry(edge.source_node_id)
                .or_default()
                .push(target);
            adjacency
                .entry(target)
                .or_default()
                .push(edge.source_node_id);
        }
    }
    let mut by_file: HashMap<Option<crate::FileId>, Vec<NodeId>> = HashMap::new();
    for node in &facts.nodes {
        by_file.entry(node.file_id).or_default().push(node.node_id);
    }
    for ids in by_file.values() {
        if ids.len() < 2 {
            continue;
        }
        for (index, left) in ids.iter().enumerate() {
            for right in &ids[index + 1..] {
                adjacency.entry(*left).or_default().push(*right);
                adjacency.entry(*right).or_default().push(*left);
            }
        }
    }

    let degree = |id: NodeId| adjacency.get(&id).map(Vec::len).unwrap_or(0);
    let mut visited: BTreeSet<NodeId> = seeds.iter().copied().collect();
    let mut frontier: VecDeque<(NodeId, usize)> = seeds.iter().copied().map(|id| (id, 0)).collect();
    let seed_set: BTreeSet<NodeId> = seeds.iter().copied().collect();
    while let Some((node, depth)) = frontier.pop_front() {
        if depth >= EXPAND_DEPTH {
            continue;
        }
        if !seed_set.contains(&node) && degree(node) >= HUB_DEGREE_FLOOR {
            continue;
        }
        for neighbor in adjacency.get(&node).into_iter().flatten() {
            if visited.insert(*neighbor) {
                frontier.push_back((*neighbor, depth + 1));
            }
        }
    }
    visited
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
        .filter(|token| {
            token.starts_with('D') && token.contains(':')
                || token.starts_with('s')
                    && token.chars().nth(1).is_some_and(|ch| ch.is_ascii_digit())
        })
        .unwrap_or_default()
        .to_string()
}
