//! Phase-1 memory retrieval harness for LoCoMo and LongMemEval-S.
//!
//! Constants come from `sol_5f73941594ed4d15` (k=10, LoCoMo eval n=1540,
//! Graphify slice 300, LongMemEval retrieval n=470, Graphify slice 50).

use spur_graph::memory_eval::{
    coverage_milli, evaluate_tasks, extractive_qa, grade_key_fact, graphify_slice,
    materialize_locomo, materialize_longmemeval, parse_locomo, parse_longmemeval, recall_at_k,
    retrieve_seed_expand, retrieve_task_ids, EvalSplit, FactVerdict, COVERED_WEIGHT,
    LME_ABSTENTION, LME_GRAPHIFY_N, LME_OFFICIAL_N, LOCOMO_ADVERSARIAL_CATEGORY,
    LOCOMO_ADVERSARIAL_COUNT, LOCOMO_GRAPHIFY_N, LOCOMO_OFFICIAL_QA, MISS_WEIGHT, PARTIAL_WEIGHT,
    RECALL_K,
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
    assert_eq!(tasks[0].gold_answer, "mental health");
}

#[test]
fn locomo_parses_numeric_gold_answer() {
    let json = r#"
    [{
      "sample_id": "conv-num",
      "conversation": {"session_1": []},
      "qa": [{
        "question": "When did Melanie paint a sunrise?",
        "answer": 2022,
        "category": 2,
        "evidence": ["D1:1"]
      }]
    }]
    "#;
    let tasks = parse_locomo(json, EvalSplit::Official).unwrap();
    assert_eq!(tasks[0].gold_answer, "2022");
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
    // Floor division on the measured LoCoMo official extractive run.
    assert_eq!(coverage_milli(257, 839, 1536), 440);
    // Floor division on the measured LongMemEval-S official extractive run.
    assert_eq!(coverage_milli(134, 198, 470), 495);
}

#[test]
fn longmemeval_parses_numeric_gold_answer() {
    let json = r#"
    [{
      "question_id": "0a995998",
      "question": "How many items of clothing do I need to pick up?",
      "answer": 3,
      "haystack_session_ids": ["answer_abc"],
      "haystack_sessions": [[{"role": "user", "content": "I need to pick up 3 items."}]],
      "answer_session_ids": ["answer_abc"]
    }]
    "#;
    let tasks = parse_longmemeval(json, EvalSplit::Official).unwrap();
    assert_eq!(tasks[0].gold_answer, "3");
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
fn qa_verdict_weights_match_solved_coverage_formula() {
    // sol_805e26de169b45b3: covered=1000, partial=500, miss=0 millipoints
    assert_eq!(COVERED_WEIGHT, 1000);
    assert_eq!(PARTIAL_WEIGHT, 500);
    assert_eq!(MISS_WEIGHT, 0);
    assert_eq!(coverage_milli(1, 0, 1), COVERED_WEIGHT);
    assert_eq!(coverage_milli(0, 1, 1), PARTIAL_WEIGHT);
    assert_eq!(coverage_milli(0, 0, 1), MISS_WEIGHT);
}

#[test]
fn grade_key_fact_covers_partial_and_miss() {
    assert_eq!(
        grade_key_fact("It raised awareness for mental health.", "mental health"),
        FactVerdict::Covered
    );
    assert_eq!(
        grade_key_fact("She talked about mental wellbeing.", "mental health"),
        FactVerdict::Partial
    );
    assert_eq!(
        grade_key_fact("I ran a charity race.", "mental health"),
        FactVerdict::Miss
    );
}

#[test]
fn extractive_qa_covers_gold_from_retrieved_context() {
    let tasks = parse_locomo(LOCOMO_FIXTURE, EvalSplit::Official).unwrap();
    let root = tempfile::tempdir().unwrap();
    materialize_locomo(LOCOMO_FIXTURE, root.path()).unwrap();
    let report = extractive_qa(root.path(), &tasks).unwrap();
    assert_eq!(report.n, 1);
    assert_eq!(report.k, RECALL_K);
    assert_eq!(report.covered, 1);
    assert_eq!(report.partial, 0);
    assert_eq!(report.miss, 0);
    assert_eq!(report.coverage_milli, COVERED_WEIGHT);
}

#[test]
fn evaluate_tasks_matches_retrieve_and_extractive_qa() {
    let tasks = parse_locomo(LOCOMO_FIXTURE, EvalSplit::Official).unwrap();
    let root = tempfile::tempdir().unwrap();
    materialize_locomo(LOCOMO_FIXTURE, root.path()).unwrap();
    let retrieve = retrieve_seed_expand(root.path(), &tasks).unwrap();
    let qa = extractive_qa(root.path(), &tasks).unwrap();
    let (report, qa2) = evaluate_tasks(root.path(), &tasks).unwrap();
    assert_eq!(report, retrieve);
    assert_eq!(qa, qa2);
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

const LME_ISOLATION_FIXTURE: &str = r#"
[
  {
    "question_id": "q1_saxophone",
    "question_type": "single-session-user",
    "question": "Where is the purple saxophone stored?",
    "answer": "Reykjavik",
    "haystack_session_ids": ["s1"],
    "haystack_sessions": [
      [{"role": "user", "content": "The purple saxophone is stored in Reykjavik."}]
    ],
    "answer_session_ids": ["s1"]
  },
  {
    "question_id": "q2_hiking",
    "question_type": "single-session-user",
    "question": "Where is the purple saxophone stored?",
    "answer": "Reykjavik",
    "haystack_session_ids": ["s9"],
    "haystack_sessions": [
      [{"role": "user", "content": "I went hiking in Oregon. No musical instruments."}]
    ],
    "answer_session_ids": ["s9"]
  }
]
"#;

#[test]
fn longmemeval_retrieve_hits_official_answer_session_ids() {
    let json = r#"
    [{
      "question_id": "e47becba",
      "question_type": "single-session-user",
      "question": "What degree did I graduate with?",
      "answer": "Business Administration",
      "haystack_session_ids": ["sharegpt_yywfIrx_0", "answer_280352e9"],
      "haystack_sessions": [
        [{"role": "user", "content": "Rachel is planning a trip."}],
        [{"role": "user", "content": "I graduated with a Business Administration degree."}]
      ],
      "answer_session_ids": ["answer_280352e9"]
    }]
    "#;
    let tasks = parse_longmemeval(json, EvalSplit::Official).unwrap();
    let root = tempfile::tempdir().unwrap();
    materialize_longmemeval(json, root.path()).unwrap();
    let hits = retrieve_task_ids(root.path(), &tasks[0]).unwrap();
    assert!(
        hits.iter().any(|id| id == "answer_280352e9"),
        "expected official answer session id, got {hits:?}"
    );
}

#[test]
fn longmemeval_retrieve_does_not_leak_across_question_haystacks() {
    // sol_4dcbe9f970c04f3d: hits.haystack_id must FK-match the queried haystack.
    // sol_e63aad30cf0e4844: a foreign haystack hit is data_integrity.foreign_key.violation.
    let tasks = parse_longmemeval(LME_ISOLATION_FIXTURE, EvalSplit::Official).unwrap();
    assert_eq!(tasks.len(), 2);
    let root = tempfile::tempdir().unwrap();
    materialize_longmemeval(LME_ISOLATION_FIXTURE, root.path()).unwrap();
    let q2_hits = retrieve_task_ids(root.path(), &tasks[1]).unwrap();
    assert!(
        !q2_hits.iter().any(|id| id == "s1"),
        "q2 must not retrieve q1-only gold s1, got {q2_hits:?}"
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
    let qa = extractive_qa(root.path(), &tasks).unwrap();
    eprintln!("locomo official retrieval {report:?}");
    eprintln!("locomo official extractive qa {qa:?}");
    assert_eq!(report.n, tasks.len());
    assert_eq!(report.k, RECALL_K);
    assert_eq!(qa.n, tasks.len());
    assert_eq!(qa.covered + qa.partial + qa.miss, qa.n as u32);
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
    eprintln!("longmemeval parsed {} tasks", tasks.len());
    let root = tempfile::tempdir().unwrap();
    materialize_longmemeval(&json, root.path()).unwrap();
    eprintln!("longmemeval materialized under {}", root.path().display());
    let (report, qa) = evaluate_tasks(root.path(), &tasks).unwrap();
    eprintln!("longmemeval official retrieval {report:?}");
    eprintln!("longmemeval official extractive qa {qa:?}");
    assert_eq!(report.n, tasks.len());
    assert_eq!(report.k, RECALL_K);
    assert_eq!(qa.n, tasks.len());
    assert_eq!(qa.covered + qa.partial + qa.miss, qa.n as u32);
}
