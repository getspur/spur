use serde_json::{json, Value};
use spur_graph::memory_eval::{
    contract::{
        validate_dataset, BenchmarkContract, BenchmarkDataset, DatasetKind, Role, SourcePin,
    },
    parse_locomo, EvalSplit, LOCOMO_ADVERSARIAL_CATEGORY, LOCOMO_GRAPHIFY_N,
};

const LOCOMO_ALL_FIELDS: &str = r#"
[
  {
    "sample_id": "conv-all",
    "conversation": {
      "speaker_a": "Alice",
      "speaker_b": "Bob",
      "session_10_date_time": "2023-01-10",
      "session_10": [
        {"speaker": "Bob", "dia_id": "D10:1", "text": "later session"}
      ],
      "session_2_date_time": "2023-01-02",
      "session_2": [
        {
          "speaker": "Alice",
          "dia_id": "D2:1",
          "text": "line one\nline two",
          "blip_caption": "race photo",
          "img_url": ["https://example.invalid/race.jpg"],
          "query": "charity race photo",
          "turn_extension": {"keep": true}
        },
        {"speaker": "Bob", "dia_id": "D2:2", "text": "That was memorable."}
      ],
      "conversation_extension": ["keep", "me"]
    },
    "qa": [
      {
        "question": "What was in the photo?",
        "answer": "a race",
        "category": 4,
        "evidence": ["D2:1"],
        "question_extension": {"keep": true}
      },
      {
        "question": "Did Alice race on Mars?",
        "category": 5,
        "evidence": [],
        "adversarial_answer": "yes, on Mars"
      },
      {
        "question": "What was never stated?",
        "answer": "nothing",
        "category": 3,
        "evidence": []
      }
    ],
    "observation": {"sample_extension": "survives"}
  }
]
"#;

const LOCOMO_UNRESOLVED_EVIDENCE: &str = r#"
[
  {
    "sample_id": "conv-unresolved",
    "conversation": {
      "speaker_a": "Alice",
      "speaker_b": "Bob",
      "session_1_date_time": "2023-01-01",
      "session_1": [
        {"speaker": "Alice", "dia_id": "D1:1", "text": "first"},
        {"speaker": "Bob", "dia_id": "D1:1", "text": "duplicate"}
      ]
    },
    "qa": [
      {
        "question": "Which evidence is valid?",
        "answer": "neither",
        "category": 4,
        "evidence": ["D9:missing", "D1:1"]
      }
    ]
  }
]
"#;

const LEGACY_LOCOMO_WITHOUT_CONVERSATION: &str = r#"
[
  {
    "sample_id": "legacy-only",
    "qa": [
      {
        "question": "Which row remains eligible?",
        "answer": "this one",
        "category": 1,
        "evidence": ["D1:1"]
      },
      {
        "question": "Which row lacks evidence?",
        "answer": "this one",
        "category": 3,
        "evidence": []
      },
      {
        "question": "Which row is adversarial?",
        "answer": "this one",
        "category": 5,
        "evidence": ["D1:2"]
      }
    ]
  }
]
"#;

const LOCOMO_REPEATED_EVIDENCE: &str = r#"
[
  {
    "sample_id": "conv-repeated",
    "conversation": {
      "speaker_a": "Alice",
      "speaker_b": "Bob",
      "session_4_date_time": "2023-01-04",
      "session_4": [
        {"speaker": "Alice", "dia_id": "D4:5", "text": "first evidence"}
      ],
      "session_5_date_time": "2023-01-05",
      "session_5": [
        {"speaker": "Bob", "dia_id": "D5:5", "text": "second evidence"}
      ]
    },
    "qa": [
      {
        "question": "Which evidence annotations support the answer?",
        "answer": "both",
        "category": 1,
        "evidence": ["D4:5", "D4:5", "D5:5"]
      }
    ]
  }
]
"#;

fn test_pin() -> SourcePin {
    SourcePin {
        origin: "https://github.com/snap-research/locomo".to_owned(),
        revision: "3eb6f2c585f5e1699204e3c3bdf7adc5c28cb376".to_owned(),
        sha256: "79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4".to_owned(),
    }
}

#[test]
fn public_locomo_loader_preserves_sessions_turns_fields_raw_json_and_all_qa_rows() {
    let source = test_pin();
    let data = BenchmarkDataset::load_locomo(LOCOMO_ALL_FIELDS, source.clone()).unwrap();

    assert_eq!(data.kind, DatasetKind::Locomo);
    assert_eq!(data.source, source);
    assert_eq!(data.raw_sha256.len(), 64);
    assert_eq!(data.conversations.len(), 1);
    assert_eq!(data.questions.len(), 3);

    let conversation = &data.conversations[0];
    assert_eq!(conversation.source_id.as_deref(), Some("conv-all"));
    assert_eq!(conversation.sessions.len(), 2);
    assert_eq!(
        conversation.raw["observation"]["sample_extension"],
        json!("survives")
    );
    assert_eq!(
        conversation.raw["conversation"]["conversation_extension"],
        json!(["keep", "me"])
    );

    let earlier = &conversation.sessions[0];
    assert_eq!(earlier.source_id.as_deref(), Some("session_2"));
    assert_eq!(earlier.occurred_at.as_deref(), Some("2023-01-02"));
    assert_eq!(earlier.turns.len(), 2);
    assert_eq!(earlier.raw[0]["query"], json!("charity race photo"));

    let turn = &earlier.turns[0];
    assert_eq!(turn.source_id.as_deref(), Some("D2:1"));
    assert_eq!(turn.role, Role::Other);
    assert_eq!(turn.speaker.as_deref(), Some("Alice"));
    assert_eq!(turn.content, "line one\nline two");
    assert_eq!(turn.caption.as_deref(), Some("race photo"));
    assert_eq!(
        turn.raw["img_url"][0],
        json!("https://example.invalid/race.jpg")
    );
    assert_eq!(turn.raw["turn_extension"]["keep"], json!(true));

    let later = &conversation.sessions[1];
    assert_eq!(later.source_id.as_deref(), Some("session_10"));
    assert_eq!(later.occurred_at.as_deref(), Some("2023-01-10"));
    assert_eq!(later.turns.len(), 1);

    let ordinary = &data.questions[0];
    assert_eq!(ordinary.category, Some(4));
    assert_eq!(ordinary.answer, json!("a race"));
    assert_eq!(ordinary.evidence.len(), 1);
    assert_eq!(ordinary.evidence[0].raw, "D2:1");
    assert_eq!(
        ordinary.evidence[0].resolved_turn_id.as_deref(),
        Some(turn.internal_id.as_str())
    );
    assert_eq!(ordinary.gold_turn_ids, vec![turn.internal_id.clone()]);
    assert_eq!(ordinary.gold_session_ids, vec![earlier.internal_id.clone()]);
    assert_eq!(ordinary.raw["question_extension"]["keep"], json!(true));

    let adversarial = &data.questions[1];
    assert_eq!(adversarial.category, Some(5));
    assert_eq!(adversarial.answer, Value::Null);
    assert_eq!(adversarial.raw["adversarial_answer"], json!("yes, on Mars"));
    assert!(adversarial.evidence.is_empty());

    assert_eq!(data.questions[2].category, Some(3));
    assert!(data.questions[2].evidence.is_empty());
}

#[test]
fn locomo_keeps_missing_and_ambiguous_evidence_unresolved() {
    let data = BenchmarkDataset::load_locomo(LOCOMO_UNRESOLVED_EVIDENCE, test_pin()).unwrap();
    let question = &data.questions[0];

    assert_eq!(question.evidence.len(), 2);
    assert_eq!(question.evidence[0].raw, "D9:missing");
    assert!(question.evidence[0].resolved_turn_id.is_none());
    assert_eq!(question.evidence[1].raw, "D1:1");
    assert!(question.evidence[1].resolved_turn_id.is_none());
    assert!(question.gold_turn_ids.is_empty());
    assert!(question.gold_session_ids.is_empty());
}

#[test]
fn locomo_preserves_repeated_evidence_but_derives_unique_ordered_gold_ids() {
    let mut data = BenchmarkDataset::load_locomo(LOCOMO_REPEATED_EVIDENCE, test_pin()).unwrap();
    data.source.sha256.clone_from(&data.raw_sha256);
    let question = &data.questions[0];
    let session_4 = &data.conversations[0].sessions[0];
    let session_5 = &data.conversations[0].sessions[1];
    let turn_d4_5 = &session_4.turns[0];
    let turn_d5_5 = &session_5.turns[0];

    assert_eq!(question.evidence.len(), 3);
    assert_eq!(question.evidence[0].raw, "D4:5");
    assert_eq!(question.evidence[1].raw, "D4:5");
    assert_eq!(question.evidence[2].raw, "D5:5");
    assert_eq!(question.raw["evidence"], json!(["D4:5", "D4:5", "D5:5"]));
    assert_eq!(
        question.gold_turn_ids,
        vec![turn_d4_5.internal_id.clone(), turn_d5_5.internal_id.clone()]
    );
    assert_eq!(
        question.gold_session_ids,
        vec![session_4.internal_id.clone(), session_5.internal_id.clone()]
    );

    let report = validate_dataset(&data, &BenchmarkContract::audited("origin-native-v1"));
    assert!(!report.has_fatal());
    assert_eq!(report.cohorts.locomo_retrieval, vec![question.id.clone()]);
}

#[test]
fn parse_locomo_retains_the_legacy_retrieval_filter() {
    assert_eq!(LOCOMO_ADVERSARIAL_CATEGORY, 5);
    assert_eq!(LOCOMO_GRAPHIFY_N, 300);

    let tasks = parse_locomo(LOCOMO_ALL_FIELDS, EvalSplit::Official).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, "conv-all#0");
    assert_eq!(tasks[0].gold_ids, vec!["D2:1"]);
    assert_eq!(tasks[0].gold_answer, "a race");
}

#[test]
fn parse_locomo_accepts_legacy_input_without_conversation() {
    let tasks = parse_locomo(LEGACY_LOCOMO_WITHOUT_CONVERSATION, EvalSplit::Official).unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, "legacy-only#0");
    assert_eq!(tasks[0].question, "Which row remains eligible?");
    assert_eq!(tasks[0].gold_ids, vec!["D1:1"]);
    assert_eq!(tasks[0].gold_answer, "this one");
}
