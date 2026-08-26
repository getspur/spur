//! LoCoMo / LongMemEval-S retrieval harness (Phase 1).
//!
//! Bounds are from `sol_5f73941594ed4d15` and
//! `sol_bca716ccfbdb404d`.

mod locomo;
mod longmemeval;
mod materialize;
mod metrics;
mod qa;
mod retrieve;

pub use locomo::parse_locomo;
pub use longmemeval::parse_longmemeval;
pub use materialize::{materialize_locomo, materialize_longmemeval};
pub use metrics::{coverage_milli, graphify_slice, recall_at_k};
pub use qa::{extractive_qa, grade_key_fact, FactVerdict, QaReport};
pub use retrieve::{retrieve_seed_expand, RetrievalReport};

/// Graphify / IR recall cutoff. `sol_5f73941594ed4d15`.
pub const RECALL_K: usize = 10;
/// LoCoMo category 5 is adversarial and is excluded from eval.
pub const LOCOMO_ADVERSARIAL_CATEGORY: u32 = 5;
/// Graphify-sized LoCoMo QA slice.
pub const LOCOMO_GRAPHIFY_N: usize = 300;
/// Official LoCoMo QA count in `locomo10.json`.
pub const LOCOMO_OFFICIAL_QA: usize = 1986;
/// Adversarial rows in that file (category 5).
pub const LOCOMO_ADVERSARIAL_COUNT: usize = 446;
/// Graphify-sized LongMemEval-S slice.
pub const LME_GRAPHIFY_N: usize = 50;
/// Official LongMemEval-S question count.
pub const LME_OFFICIAL_N: usize = 500;
/// Abstention items skipped for retrieval.
pub const LME_ABSTENTION: usize = 30;
/// Graphify BFS hub floor (`max(50, p99)`).
pub const HUB_DEGREE_FLOOR: usize = 50;
/// Graphify default expand depth.
pub const EXPAND_DEPTH: usize = 3;
/// Graphify key-fact millipoints. `sol_805e26de169b45b3`.
pub const COVERED_WEIGHT: u32 = 1000;
pub const PARTIAL_WEIGHT: u32 = 500;
pub const MISS_WEIGHT: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalSplit {
    Official,
    Graphify,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryTask {
    pub id: String,
    pub question: String,
    pub gold_ids: Vec<String>,
    pub gold_answer: String,
}
