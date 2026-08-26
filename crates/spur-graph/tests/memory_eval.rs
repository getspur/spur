//! Phase-1 memory retrieval harness for LoCoMo and LongMemEval-S.
//!
//! Constants come from `sol_5f73941594ed4d15` (k=10, LoCoMo eval n=1540,
//! Graphify slice 300, LongMemEval retrieval n=470, Graphify slice 50).

use spur_graph::memory_eval::{
    coverage_milli, graphify_slice, materialize_locomo, materialize_longmemeval, parse_locomo,
    parse_longmemeval, recall_at_k, retrieve_seed_expand, EvalSplit, LME_ABSTENTION,
    LME_GRAPHIFY_N, LME_OFFICIAL_N, LOCOMO_ADVERSARIAL_CATEGORY, LOCOMO_ADVERSARIAL_COUNT,
    LOCOMO_GRAPHIFY_N, LOCOMO_OFFICIAL_QA, RECALL_K,
};

const LOCOMO_FIXTURE: &str = r#"
[
  {
    "sample_id": "conv-26",
    "conversation": {
      "speaker_a": "Alice",
      "speaker_b": "Bob",
      "session_1_date_time": "2023-01-01",
      "session_1": [
        {"speaker": "Alice", "dia_id": "D1:1", "text": "I ran a charity race."},
        {"speaker": "Bob", "dia_id": "D1:2", "text": "What did the charity race raise awareness for?"},
        {"speaker": "Alice", "dia_id": "D1:3", "text": "It raised awareness for mental health."}
      ]
    },
    "qa": [
      {
        "question": "What did the charity race raise awareness for?",
        "answer": "mental health",
        "category": 4,
        "evidence": ["D1:3"]
      },
      {
        "question": "Did Alice invent a fake memory about space travel?",
        "answer": "no",
        "category": 5,
        "evidence": []
      }
    ]
  }
]
"#;

const LME_FIXTURE: &str = r#"
[
  {
    "question_id": "gpt4_fe651585",
    "question_type": "single-session-user",
    "question": "Who became a parent first, Rachel or Alex?",
    "answer": "Alex",
    "haystack_session_ids": ["s1", "s2"],
    "haystack_sessions": [
      [
        {"role": "user", "content": "Rachel is planning a trip."}
      ],
      [
        {"role": "user", "content": "Alex became a parent last spring.", "has_answer": true},
        {"role": "assistant", "content": "Congratulations to Alex."}
      ]
    ],
    "answer_session_ids": ["s2"]
  },
  {
    "question_id": "gpt4_abs_0001_abs",
    "question_type": "temporal-reasoning",
    "question": "When did I buy a yacht?",
    "answer": "unknown",
    "haystack_session_ids": ["s1"],
    "haystack_sessions": [
      [{"role": "user", "content": "I bought groceries."}]
    ],
    "answer_session_ids": []
  }
]
"#;

#[test]
fn recall_k_is_the_solved_bound() {
    assert_eq!(RECALL_K, 10);
}

#[test]
fn recall_at_k_is_gold_hits_over_gold_total() {
    // sat model: recall_milli * gold_total = 1000 * gold_hits
    let milli = recall_at_k(&["D1:3", "D9:9"], &["D1:3", "other"], RECALL_K);
    assert_eq!(milli, 500);
}

#[test]
fn locomo_drops_adversarial_category_five() {
    assert_eq!(LOCOMO_ADVERSARIAL_CATEGORY, 5);
    let tasks = parse_locomo(LOCOMO_FIXTURE, EvalSplit::Official).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].question,
        "What did the charity race raise awareness for?"
    );
    assert_eq!(tasks[0].gold_ids, vec!["D1:3"]);
}

#[test]
fn locomo_graphify_slice_is_at_most_three_hundred() {
    assert_eq!(LOCOMO_GRAPHIFY_N, 300);
    let tasks: Vec<u32> = (0..1540).collect();
    let slice = graphify_slice(&tasks, LOCOMO_GRAPHIFY_N);
    assert_eq!(slice.len(), 300);
    assert_eq!(slice[0], 0);
    assert_eq!(slice[299], 299);
}

#[test]
fn coverage_milli_uses_graphify_partial_credit() {
    // coverage_milli * total = 1000 * covered + 500 * partial
    assert_eq!(coverage_milli(4, 1, 6), 750);
}

#[test]
fn longmemeval_skips_abstention_ids_for_retrieval() {
    let tasks = parse_longmemeval(LME_FIXTURE, EvalSplit::Official).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, "gpt4_fe651585");
    assert_eq!(tasks[0].gold_ids, vec!["s2"]);
    assert_eq!(LME_GRAPHIFY_N, 50);
}

#[test]
fn locomo_official_eval_drops_adversarial_count() {
    assert_eq!(LOCOMO_OFFICIAL_QA - LOCOMO_ADVERSARIAL_COUNT, 1540);
    assert_eq!(LME_OFFICIAL_N - LME_ABSTENTION, 470);
}

#[test]
fn locomo_materialize_then_retrieve_hits_gold_turn() {
    let tasks = parse_locomo(LOCOMO_FIXTURE, EvalSplit::Official).unwrap();
    let root = tempfile::tempdir().unwrap();
    materialize_locomo(LOCOMO_FIXTURE, root.path()).unwrap();
    let report = retrieve_seed_expand(root.path(), &tasks).unwrap();
    assert_eq!(report.n, 1);
    assert!(
        report.mean_recall_milli > 0,
        "expected a gold hit, got {report:?}"
    );
    assert_eq!(report.k, RECALL_K);
}

#[test]
fn longmemeval_materialize_then_retrieve_hits_gold_session() {
    let tasks = parse_longmemeval(LME_FIXTURE, EvalSplit::Official).unwrap();
    let root = tempfile::tempdir().unwrap();
    materialize_longmemeval(LME_FIXTURE, root.path()).unwrap();
    let report = retrieve_seed_expand(root.path(), &tasks).unwrap();
    assert_eq!(report.n, 1);
    assert!(
        report.mean_recall_milli > 0,
        "expected a gold session hit, got {report:?}"
    );
}

#[test]
#[ignore = "set SPUR_LOCOMO_JSON to locomo10.json; CC BY-NC, do not vendor"]
fn locomo_official_from_env() {
    let path = std::env::var("SPUR_LOCOMO_JSON").expect("SPUR_LOCOMO_JSON");
    let json = std::fs::read_to_string(&path).unwrap();
    let tasks = parse_locomo(&json, EvalSplit::Official).unwrap();
    assert!(
        tasks.len() >= LOCOMO_GRAPHIFY_N,
        "expected at least the Graphify-sized slice, got {}",
        tasks.len()
    );
    let root = tempfile::tempdir().unwrap();
    materialize_locomo(&json, root.path()).unwrap();
    let report = retrieve_seed_expand(root.path(), &tasks).unwrap();
    eprintln!("locomo official {report:?}");
    assert_eq!(report.n, tasks.len());
    assert_eq!(report.k, RECALL_K);
}

#[test]
#[ignore = "set SPUR_LONGMEMEVAL_JSON to longmemeval_s_cleaned.json"]
fn longmemeval_official_from_env() {
    let path = std::env::var("SPUR_LONGMEMEVAL_JSON").expect("SPUR_LONGMEMEVAL_JSON");
    let json = std::fs::read_to_string(&path).unwrap();
    let tasks = parse_longmemeval(&json, EvalSplit::Official).unwrap();
    assert!(
        tasks.len() >= LME_GRAPHIFY_N,
        "expected at least the Graphify-sized slice, got {}",
        tasks.len()
    );
    let root = tempfile::tempdir().unwrap();
    materialize_longmemeval(&json, root.path()).unwrap();
    let report = retrieve_seed_expand(root.path(), &tasks).unwrap();
    eprintln!("longmemeval official {report:?}");
    assert_eq!(report.n, tasks.len());
    assert_eq!(report.k, RECALL_K);
}
