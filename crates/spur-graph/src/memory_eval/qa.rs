use std::path::Path;

use crate::build_facts;

use super::retrieve::retrieve_task_hits;
use super::{coverage_milli, MemoryTask, RECALL_K};

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
    let facts = build_facts(root, None)?.0;
    let mut covered = 0u32;
    let mut partial = 0u32;
    let mut miss = 0u32;
    for task in tasks {
        let hits = retrieve_task_hits(&facts, &task.question);
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
