use serde_json::{json, Value};
use spur_graph::memory_eval::{
    contract::{BenchmarkDataset, SourcePin},
    qa::{
        evaluate_locomo, locomo_adversarial_options, ranking_sha256,
        released_adversarial_answer_compatibility_shim, render_locomo_prompt,
        render_locomo_prompt_with_seed, score_locomo, validate_qa_ranking_hash, AdversarialChoice,
        QaBackend, QaRequest, QaResponse, QaStatus,
    },
    ranking::{
        Granularity, QueryOccurrenceId, RankRequest, Ranker, Ranking, RankingSet, RecentRanker,
        Variant,
    },
};

const LOCOMO_QA_FIXTURE: &str = r#"
[
  {
    "sample_id": "conv-qa",
    "conversation": {
      "speaker_a": "Alice",
      "speaker_b": "Bob",
      "session_1_date_time": "2023-01-01",
      "session_1": [
        {"speaker": "Bob", "dia_id": "D1:1", "text": "Earlier context."}
      ],
      "session_2_date_time": "2023-01-02",
      "session_2": [
        {
          "speaker": "Alice",
          "dia_id": "D2:1",
          "text": "line one\nline two",
          "blip_caption": "race photo"
        }
      ]
    },
    "qa": [
      {
        "question": "When did Alice share the photo?",
        "answer": "January 2, 2023",
        "category": 2,
        "evidence": ["D2:1"]
      },
      {
        "question": "Did Alice race on Mars?",
        "category": 5,
        "evidence": [],
        "adversarial_answer": "Yes, Alice raced on Mars"
      }
    ]
  }
]
"#;

fn fixture_dataset() -> BenchmarkDataset {
    BenchmarkDataset::load_locomo(
        LOCOMO_QA_FIXTURE,
        SourcePin {
            origin: "https://github.com/snap-research/locomo".to_owned(),
            revision: "3eb6f2c585f5e1699204e3c3bdf7adc5c28cb376".to_owned(),
            sha256: "79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4".to_owned(),
        },
    )
    .unwrap()
}

fn frozen_ranking(dataset: &BenchmarkDataset, question_index: usize) -> Ranking {
    let question = &dataset.questions[question_index];
    let corpus = dataset
        .all_sessions()
        .flat_map(|session| {
            session.turns.iter().map(
                move |turn| spur_graph::memory_eval::ranking::CorpusDocument {
                    occurrence_id: turn.internal_id.clone(),
                    text: turn.content.clone(),
                    chronology_key: Some(spur_graph::memory_eval::ranking::ChronologyKey::new(
                        if session.occurred_at.as_deref() == Some("2023-01-02") {
                            2
                        } else {
                            1
                        },
                    )),
                },
            )
        })
        .collect::<Vec<_>>();
    RecentRanker
        .rank(
            &RankRequest {
                query: &question.text,
                granularity: Granularity::Turn,
                corpus: &corpus,
            },
            2,
        )
        .unwrap()
}

#[test]
fn locomo_prompt_is_origin_golden_and_keeps_date_speaker_caption_and_multiline_text() {
    let dataset = fixture_dataset();
    let prompt = render_locomo_prompt(
        &dataset.questions[0],
        &frozen_ranking(&dataset, 0),
        &dataset,
    )
    .unwrap();

    assert_eq!(
        prompt,
        concat!(
            "Below is a conversation between two people: Alice and Bob. ",
            "The conversation takes place over multiple days and the date of each conversation ",
            "is wriiten at the beginning of the conversation.\n\n",
            "DATE: 2023-01-02\n",
            "CONVERSATION:\n",
            "Alice said, \"line one\nline two\"\n",
            " and shared race photo.\n\n",
            "DATE: 2023-01-01\n",
            "CONVERSATION:\n",
            "Bob said, \"Earlier context.\"\n\n",
            "Based on the above context, write an answer in the form of a short phrase for the ",
            "following question. Answer with exact words from the context whenever possible.\n",
            "Question: When did Alice share the photo? Use DATE of CONVERSATION to answer with ",
            "an approximate date. Short answer:"
        )
    );
}

#[test]
fn locomo_category_scorers_match_origin_golden_cases() {
    assert_eq!(score_locomo(1, "Alice; Bob", json!(["Alice", "Bob"])), 1.0);
    assert_eq!(score_locomo(2, "running races", json!("ran a race")), 1.0);
    assert_eq!(
        score_locomo(3, "blue bicycle", json!("blue bicycle; blue bike")),
        1.0
    );
    assert_eq!(
        score_locomo(4, "The blue bicycles and cars.", json!("blue bicycle car")),
        1.0
    );
    assert_eq!(score_locomo(5, "no", json!("no")), 1.0);
    assert_eq!(
        score_locomo(5, "Not mentioned in the conversation", Value::Null),
        1.0
    );
    assert_eq!(score_locomo(5, "Yes, on Mars", Value::Null), 0.0);
}

#[test]
fn released_adversarial_answer_shim_and_seeded_options_are_explicit_and_deterministic() {
    let dataset = fixture_dataset();
    let question = &dataset.questions[1];

    assert_eq!(
        released_adversarial_answer_compatibility_shim(question).unwrap(),
        "Yes, Alice raced on Mars"
    );

    let first = locomo_adversarial_options(question, 17).unwrap();
    let replay = locomo_adversarial_options(question, 17).unwrap();
    assert_eq!(first, replay);
    assert_eq!(first.correct_text(), "Not mentioned in the conversation");
    assert_eq!(first.false_text(), "Yes, Alice raced on Mars");

    let observed_choices = (0..64)
        .map(|seed| locomo_adversarial_options(question, seed).unwrap().correct)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        observed_choices,
        std::collections::BTreeSet::from([AdversarialChoice::A, AdversarialChoice::B])
    );

    let prompt =
        render_locomo_prompt_with_seed(question, &frozen_ranking(&dataset, 1), &dataset, 17)
            .unwrap();
    assert!(prompt.contains("Select the correct answer: (a)"));
    assert!(prompt.contains("(b)"));
    assert!(prompt.contains("Not mentioned in the conversation"));
    assert!(prompt.contains("Yes, Alice raced on Mars"));
}

#[derive(Default)]
struct RecordingBackend {
    requests: Vec<QaRequest>,
}

impl QaBackend for RecordingBackend {
    fn complete(&mut self, request: &QaRequest) -> anyhow::Result<QaResponse> {
        self.requests.push(request.clone());
        Ok(QaResponse {
            output_text: "January 2, 2023".to_owned(),
            input_tokens: 101,
            output_tokens: 7,
        })
    }
}

fn one_question_rankings(dataset: &BenchmarkDataset, ranking: Ranking) -> RankingSet {
    let mut rankings = RankingSet::new();
    rankings.insert(
        (
            QueryOccurrenceId::new(dataset.questions[0].id.clone()),
            Variant::Recent,
            Granularity::Turn,
        ),
        ranking,
    );
    rankings
}

#[test]
fn qa_backend_consumes_frozen_ranking_and_binds_request_and_record_to_its_hash() {
    let dataset = fixture_dataset();
    let ranking = frozen_ranking(&dataset, 0);
    let expected_hash = ranking_sha256(&ranking).unwrap();
    let rankings = one_question_rankings(&dataset, ranking);
    let frozen_before = rankings.clone();
    let mut backend = RecordingBackend::default();

    let records = evaluate_locomo(&dataset, &rankings, &mut backend, 17).unwrap();

    assert_eq!(rankings, frozen_before, "QA mutated or reranked context");
    assert_eq!(records.len(), 1);
    assert_eq!(backend.requests.len(), 1);
    assert_eq!(records[0].question_id, dataset.questions[0].id);
    assert_eq!(records[0].status, QaStatus::Complete);
    assert_eq!(records[0].ranking_sha256, expected_hash);
    assert_eq!(backend.requests[0].ranking_sha256, expected_hash);
    assert_eq!(records[0].prompt_sha256, backend.requests[0].prompt_sha256);
    assert_eq!(records[0].recorded_seed, 17);
    assert_eq!(records[0].input_tokens, 101);
    assert_eq!(records[0].output_tokens, 7);
    assert_eq!(records[0].score, 1.0);
    validate_qa_ranking_hash(&records[0], &expected_hash).unwrap();
    assert!(validate_qa_ranking_hash(&records[0], &"0".repeat(64)).is_err());
}

#[test]
fn caller_owned_question_key_cannot_bind_a_ranking_for_different_query_text() {
    let dataset = fixture_dataset();
    let wrong_ranking = frozen_ranking(&dataset, 1);
    let rankings = one_question_rankings(&dataset, wrong_ranking);
    let mut backend = RecordingBackend::default();

    let error = evaluate_locomo(&dataset, &rankings, &mut backend, 17).unwrap_err();

    assert!(error.to_string().contains("query hash"), "{error:#}");
    assert!(backend.requests.is_empty());
}
