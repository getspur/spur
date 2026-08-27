use serde_json::json;
use spur_graph::memory_eval::contract::{BenchmarkDataset, Role, SourcePin};

fn test_pin() -> SourcePin {
    SourcePin {
        origin: "fixture://longmemeval".to_owned(),
        revision: "test".to_owned(),
        sha256: "fixture".to_owned(),
    }
}

#[test]
fn preserves_roles_dates_multiline_raw_fields_and_independent_gold() {
    let json = json!([
        {
            "question_id": "q-all-fields",
            "question_type": "single-session-assistant",
            "question": "What did the assistant remember?",
            "answer": {"text": "first line\nsecond line"},
            "question_date": "2024-02-01",
            "haystack_session_ids": ["session-gold", "session-turn-gold"],
            "haystack_dates": ["2024-01-01", "2024-01-02"],
            "haystack_sessions": [
                [
                    {
                        "role": "user",
                        "content": "session-level evidence",
                        "has_answer": false,
                        "turn_extra": "preserved"
                    }
                ],
                [
                    {
                        "role": "assistant",
                        "content": "first line\nsecond line",
                        "has_answer": true
                    }
                ]
            ],
            "answer_session_ids": ["session-gold"],
            "source_extra": {"preserved": true}
        }
    ])
    .to_string();

    let data = BenchmarkDataset::load_longmemeval(&json, test_pin()).unwrap();
    let question = &data.questions[0];
    assert_eq!(question.question_date.as_deref(), Some("2024-02-01"));
    assert_eq!(
        question.question_type.as_deref(),
        Some("single-session-assistant")
    );
    assert_eq!(question.answer, json!({"text": "first line\nsecond line"}));
    assert_eq!(question.raw["source_extra"]["preserved"], true);
    assert_eq!(question.gold_session_ids.len(), 1);
    assert_eq!(question.gold_turn_ids.len(), 1);

    let answer_turn = data.turn(&question.gold_turn_ids[0]).unwrap();
    assert_eq!(answer_turn.role, Role::Assistant);
    assert_eq!(answer_turn.content, "first line\nsecond line");
    assert_eq!(answer_turn.has_answer, Some(true));

    let sessions = data.all_sessions().collect::<Vec<_>>();
    assert_eq!(sessions[0].occurred_at.as_deref(), Some("2024-01-01"));
    assert_eq!(sessions[1].occurred_at.as_deref(), Some("2024-01-02"));
    assert_eq!(sessions[0].turns[0].role, Role::User);
    assert_eq!(sessions[0].turns[0].has_answer, Some(false));
    assert_eq!(sessions[0].turns[0].raw["turn_extra"], "preserved");
    assert_eq!(question.gold_session_ids[0], sessions[0].internal_id);
    assert!(sessions[1]
        .turns
        .iter()
        .any(|turn| turn.internal_id == question.gold_turn_ids[0]));
    assert_ne!(question.gold_session_ids[0], sessions[1].internal_id);
}

#[test]
fn repeated_source_session_ids_remain_distinct_occurrences() {
    let json = json!([
        {
            "question_id": "q-repeated",
            "question_type": "knowledge-update",
            "question": "Which occurrence is evidence?",
            "answer": "both source matches remain provenance",
            "question_date": "2024-02-01",
            "haystack_session_ids": ["shared", "shared"],
            "haystack_dates": ["2024-01-01", "2024-01-02"],
            "haystack_sessions": [
                [{"role": "user", "content": "same text"}],
                [{"role": "user", "content": "same text"}]
            ],
            "answer_session_ids": ["shared"]
        }
    ])
    .to_string();

    let data = BenchmarkDataset::load_longmemeval(&json, test_pin()).unwrap();
    let sessions = data.all_sessions().collect::<Vec<_>>();
    assert_eq!(sessions[0].source_id, sessions[1].source_id);
    assert_ne!(sessions[0].internal_id, sessions[1].internal_id);
    assert_ne!(sessions[0].occurred_at, sessions[1].occurred_at);
    assert_ne!(
        sessions[0].turns[0].internal_id,
        sessions[1].turns[0].internal_id
    );
    assert_eq!(
        data.questions[0].gold_session_ids,
        vec![
            sessions[0].internal_id.clone(),
            sessions[1].internal_id.clone()
        ]
    );
}

#[test]
fn rejects_each_parallel_haystack_array_mismatch_without_truncation() {
    let cases = [
        (json!(["s0", "s1"]), json!(["d0"]), json!([[]])),
        (json!(["s0"]), json!(["d0", "d1"]), json!([[]])),
        (json!(["s0"]), json!(["d0"]), json!([[], []])),
    ];

    for (session_ids, dates, sessions) in cases {
        let json = json!([
            {
                "question_id": "q-mismatch",
                "question_type": "single-session-user",
                "question": "Will this fail closed?",
                "answer": "yes",
                "question_date": "2024-02-01",
                "haystack_session_ids": session_ids,
                "haystack_dates": dates,
                "haystack_sessions": sessions,
                "answer_session_ids": []
            }
        ])
        .to_string();

        let error = BenchmarkDataset::load_longmemeval(&json, test_pin()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("parallel haystack arrays differ for q-mismatch"));
        assert!(message.contains("ids="));
        assert!(message.contains("dates="));
        assert!(message.contains("sessions="));
    }
}

#[test]
fn keeps_all_500_questions_including_abstention_rows() {
    let items = (0..500)
        .map(|index| {
            let question_id = if index < 30 {
                format!("q-{index:03}_abs")
            } else {
                format!("q-{index:03}")
            };
            json!({
                "question_id": question_id,
                "question_type": "single-session-user",
                "question": "fixture question",
                "answer": "fixture answer",
                "question_date": "2024-02-01",
                "haystack_session_ids": [],
                "haystack_dates": [],
                "haystack_sessions": [],
                "answer_session_ids": []
            })
        })
        .collect::<Vec<_>>();
    let json = serde_json::to_string(&items).unwrap();

    let data = BenchmarkDataset::load_longmemeval(&json, test_pin()).unwrap();
    assert_eq!(data.questions.len(), 500);
    assert_eq!(
        data.questions
            .iter()
            .filter(|question| question.id.ends_with("_abs"))
            .count(),
        30
    );
}
