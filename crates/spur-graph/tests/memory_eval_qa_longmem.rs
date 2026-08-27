use std::collections::VecDeque;

use anyhow::anyhow;
use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spur_graph::memory_eval::{
    contract::{BenchmarkDataset, SourcePin},
    qa::{
        build_longmem_reader_request, evaluate_longmem, render_longmem_judge_prompt, JsonQaCache,
        LongMemQaBackend, LongMemQaRequest, LongMemQaResponse, OpenAiResponsesBackend, QaBudget,
        QaBudgetLimits, QaCache, QaCacheKey, QaStage, QaStatus, QaUsage, LONGMEMEVAL_MODEL,
        OPENAI_RESPONSES_URL,
    },
    ranking::{Granularity, QueryOccurrenceId, RankedHit, Ranking, RankingSet, Variant},
};

const LONGMEM_FIXTURE: &str = r#"
[
  {
    "question_id": "q-chronology",
    "question_type": "multi-session",
    "question": "What did the user decide?",
    "answer": "Take the train",
    "question_date": "2024/01/03 (Wed) 09:00",
    "haystack_session_ids": ["later", "earlier"],
    "haystack_dates": ["2024/01/02 (Tue) 10:00", "2024/01/01 (Mon) 08:00"],
    "haystack_sessions": [
      [
        {"role": "user", "content": "I will take\nthe train.", "has_answer": true},
        {"role": "assistant", "content": "That sounds efficient."}
      ],
      [
        {"role": "user", "content": "I need to travel tomorrow."},
        {"role": "assistant", "content": "We can compare routes."}
      ]
    ],
    "answer_session_ids": ["later"]
  }
]
"#;

fn fixture_dataset() -> BenchmarkDataset {
    BenchmarkDataset::load_longmemeval(
        LONGMEM_FIXTURE,
        SourcePin {
            origin: "https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned".into(),
            revision: "98d7416c24c778c2fee6e6f3006e7a073259d48f".into(),
            sha256: "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442".into(),
        },
    )
    .unwrap()
}

fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn frozen_session_ranking(dataset: &BenchmarkDataset) -> Ranking {
    let conversation = &dataset.conversations[0];
    Ranking {
        variant: Variant::Recent,
        granularity: Granularity::Session,
        k: 2,
        // Deliberately rank newest-first. The origin reader presents selected
        // immutable chunks chronologically without mutating this artifact.
        hits: conversation
            .sessions
            .iter()
            .map(|session| RankedHit {
                occurrence_id: session.internal_id.clone(),
                provenance_id: None,
                score: 1.0,
            })
            .collect(),
        query_sha256: sha256(&dataset.questions[0].text),
        corpus_sha256: "corpus-hash".into(),
        serialization_sha256: "serialization-hash".into(),
    }
}

fn frozen_rankings(dataset: &BenchmarkDataset) -> RankingSet {
    let mut rankings = RankingSet::new();
    rankings.insert(
        (
            QueryOccurrenceId::new(dataset.questions[0].id.clone()),
            Variant::Recent,
            Granularity::Session,
        ),
        frozen_session_ranking(dataset),
    );
    rankings
}

fn response(output_text: &str, input: u64, output: u64) -> LongMemQaResponse {
    LongMemQaResponse {
        output_text: output_text.into(),
        usage: QaUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
        },
        raw_response: json!({
            "status": "completed",
            "output_text": output_text,
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "total_tokens": input + output
            }
        }),
    }
}

fn budget(max_requests: u64) -> QaBudget {
    QaBudget::new(QaBudgetLimits {
        max_requests,
        max_total_tokens: 10_000,
        max_usd_micros: 10_000,
        reserve_tokens_per_request: 1,
        reserve_usd_micros_per_request: 1,
        input_usd_micros_per_million: 1,
        output_usd_micros_per_million: 1,
    })
}

#[derive(Default)]
struct FakeBackend {
    replies: VecDeque<anyhow::Result<LongMemQaResponse>>,
    requests: Vec<LongMemQaRequest>,
}

impl FakeBackend {
    fn scripted(replies: impl IntoIterator<Item = anyhow::Result<LongMemQaResponse>>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            requests: Vec::new(),
        }
    }
}

#[async_trait]
impl LongMemQaBackend for FakeBackend {
    async fn complete(&mut self, request: &LongMemQaRequest) -> anyhow::Result<LongMemQaResponse> {
        self.requests.push(request.clone());
        self.replies
            .pop_front()
            .unwrap_or_else(|| Err(anyhow!("unexpected backend request")))
    }
}

#[derive(Default)]
struct FailingPutCache;

impl QaCache for FailingPutCache {
    fn get(
        &self,
        _key: &QaCacheKey,
    ) -> anyhow::Result<Option<spur_graph::memory_eval::qa::QaCacheEntry>> {
        Ok(None)
    }

    fn put(&mut self, _entry: &spur_graph::memory_eval::qa::QaCacheEntry) -> anyhow::Result<()> {
        Err(anyhow!("injected cache put failure"))
    }
}

#[test]
fn longmem_prompt_is_chronological_json_with_both_roles_and_all_dates() {
    let dataset = fixture_dataset();
    let ranking = frozen_session_ranking(&dataset);

    let request = build_longmem_reader_request(&dataset.questions[0], &ranking, &dataset).unwrap();

    assert_eq!(request.model, LONGMEMEVAL_MODEL);
    assert_eq!(request.stage, QaStage::Reader);
    let earlier = request.prompt.find("2024/01/01 (Mon) 08:00").unwrap();
    let later = request.prompt.find("2024/01/02 (Tue) 10:00").unwrap();
    assert!(earlier < later, "reader history was not chronological");
    assert!(request.prompt.contains(r#""role":"user""#));
    assert!(request.prompt.contains(r#""role":"assistant""#));
    assert!(request.prompt.contains(r#"I will take\nthe train."#));
    assert!(request
        .prompt
        .contains("Current Date: 2024/01/03 (Wed) 09:00"));
    assert!(request.prompt.contains(
        "first extract all the relevant information, and then reason over the information"
    ));
}

#[test]
fn longmem_judge_prompt_replays_origin_task_specific_and_abstention_contracts() {
    let temporal = render_longmem_judge_prompt(
        "temporal-reasoning",
        "q",
        &json!("18 days"),
        "19 days",
        false,
    )
    .unwrap();
    assert!(temporal.contains("do not penalize off-by-one errors"));
    assert!(temporal.contains("Correct Answer: 18 days"));

    let abstention = render_longmem_judge_prompt(
        "single-session-user",
        "q",
        &json!("The history never says."),
        "I do not know",
        true,
    )
    .unwrap();
    assert!(abstention.contains("unanswerable question"));
    assert!(abstention.contains("Explanation: The history never says."));
}

#[test]
fn responses_adapter_shape_and_decoder_enforce_the_audited_contract() {
    let dataset = fixture_dataset();
    let request = build_longmem_reader_request(
        &dataset.questions[0],
        &frozen_session_ranking(&dataset),
        &dataset,
    )
    .unwrap();
    let body = OpenAiResponsesBackend::request_json(&request).unwrap();

    assert_eq!(OPENAI_RESPONSES_URL, "https://api.openai.com/v1/responses");
    assert_eq!(body["model"], LONGMEMEVAL_MODEL);
    assert_eq!(body["input"], request.prompt);
    assert_eq!(body["store"], false);

    let decoded = OpenAiResponsesBackend::decode_response(
        200,
        &serde_json::to_vec(&response("answer", 12, 3).raw_response).unwrap(),
    )
    .unwrap();
    assert_eq!(decoded.output_text, "answer");
    assert_eq!(decoded.usage.total_tokens, 15);

    let invalid = [
        (500, json!({"error": {"message": "boom"}})),
        (
            200,
            json!({
                "status": "in_progress",
                "output_text": "answer",
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
            }),
        ),
        (
            200,
            json!({
                "status": "completed",
                "output_text": "  ",
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
            }),
        ),
        (
            200,
            json!({
                "status": "completed",
                "output_text": "answer",
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 3}
            }),
        ),
        (
            200,
            json!({
                "status": "completed",
                "output_text": "answer",
                "usage": {"input_tokens": "one", "output_tokens": 1, "total_tokens": 2}
            }),
        ),
    ];
    for (status, body) in invalid {
        assert!(
            OpenAiResponsesBackend::decode_response(status, &serde_json::to_vec(&body).unwrap())
                .is_err(),
            "accepted invalid response: {body}"
        );
    }
}

#[test]
fn cache_key_changes_for_question_prompt_model_ranking_and_stage() {
    let base = QaCacheKey::new(
        "q",
        "question-hash",
        "prompt-hash",
        LONGMEMEVAL_MODEL,
        "ranking-hash",
        Variant::Recent,
        Granularity::Session,
        QaStage::Reader,
    );
    let changed = [
        QaCacheKey::new(
            "q2",
            "question-hash-2",
            "prompt-hash",
            LONGMEMEVAL_MODEL,
            "ranking-hash",
            Variant::Recent,
            Granularity::Session,
            QaStage::Reader,
        ),
        QaCacheKey::new(
            "q",
            "question-hash",
            "prompt-hash-2",
            LONGMEMEVAL_MODEL,
            "ranking-hash",
            Variant::Recent,
            Granularity::Session,
            QaStage::Reader,
        ),
        QaCacheKey::new(
            "q",
            "question-hash",
            "prompt-hash",
            "different-model",
            "ranking-hash",
            Variant::Recent,
            Granularity::Session,
            QaStage::Reader,
        ),
        QaCacheKey::new(
            "q",
            "question-hash",
            "prompt-hash",
            LONGMEMEVAL_MODEL,
            "ranking-hash-2",
            Variant::Recent,
            Granularity::Session,
            QaStage::Reader,
        ),
        QaCacheKey::new(
            "q",
            "question-hash",
            "prompt-hash",
            LONGMEMEVAL_MODEL,
            "ranking-hash",
            Variant::Recent,
            Granularity::Session,
            QaStage::Judge,
        ),
    ];

    assert_eq!(base.question_sha256, "question-hash");
    assert_eq!(base.prompt_sha256, "prompt-hash");
    assert_eq!(base.model, LONGMEMEVAL_MODEL);
    assert_eq!(base.model_sha256, sha256(LONGMEMEVAL_MODEL));
    assert_eq!(base.ranking_sha256, "ranking-hash");
    assert!(changed.iter().all(|key| key != &base));
    assert_eq!(
        changed
            .iter()
            .map(QaCacheKey::identity_sha256)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        changed.len()
    );
}

#[tokio::test]
async fn successful_reader_and_judge_are_fully_cached_and_resume_without_backend_calls() {
    let dataset = fixture_dataset();
    let rankings = frozen_rankings(&dataset);
    let frozen_before = rankings.clone();
    let temp = tempfile::tempdir().unwrap();
    let mut cache = JsonQaCache::open(temp.path()).unwrap();
    let mut first_backend = FakeBackend::scripted([
        Ok(response("Take the train", 100, 8)),
        Ok(response("yes", 40, 1)),
    ]);

    let first = evaluate_longmem(
        &dataset,
        &rankings,
        &mut first_backend,
        &mut cache,
        &mut budget(2),
    )
    .await
    .unwrap();

    assert_eq!(rankings, frozen_before, "QA mutated or reranked context");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, QaStatus::Complete);
    assert_eq!(first[0].label, Some(true));
    assert_eq!(first[0].hypothesis.as_deref(), Some("Take the train"));
    assert_eq!(first_backend.requests.len(), 2);
    assert!(first_backend
        .requests
        .iter()
        .all(|request| request.model == LONGMEMEVAL_MODEL));
    assert_eq!(first_backend.requests[0].stage, QaStage::Reader);
    assert_eq!(first_backend.requests[1].stage, QaStage::Judge);

    let reader_key = QaCacheKey::from_request(&first_backend.requests[0]);
    let judge_key = QaCacheKey::from_request(&first_backend.requests[1]);
    let reader_entry = cache.get(&reader_key).unwrap().unwrap();
    let judge_entry = cache.get(&judge_key).unwrap().unwrap();
    assert_eq!(reader_entry.response.output_text, "Take the train");
    assert!(judge_entry
        .request
        .prompt
        .contains("Model Response: Take the train"));
    assert_eq!(judge_entry.response.raw_response["output_text"], "yes");
    assert_eq!(judge_entry.label, Some(true));

    drop(cache);
    let mut reopened = JsonQaCache::open(temp.path()).unwrap();
    let mut resume_backend = FakeBackend::default();
    let mut resume_budget = budget(2);
    let resumed = evaluate_longmem(
        &dataset,
        &rankings,
        &mut resume_backend,
        &mut reopened,
        &mut resume_budget,
    )
    .await
    .unwrap();
    assert_eq!(resumed, first);
    assert!(resume_backend.requests.is_empty());
    assert_eq!(resume_budget.requests(), 2);
    assert_eq!(resume_budget.usage().total_tokens, 149);
}

#[tokio::test]
async fn reader_or_judge_failure_retains_pending_denominator_without_a_label() {
    let dataset = fixture_dataset();
    let rankings = frozen_rankings(&dataset);
    let temp = tempfile::tempdir().unwrap();

    let mut reader_cache = JsonQaCache::open(temp.path().join("reader")).unwrap();
    let mut reader_failure = FakeBackend::scripted([Err(anyhow!("missing OPENAI_API_KEY"))]);
    let reader_records = evaluate_longmem(
        &dataset,
        &rankings,
        &mut reader_failure,
        &mut reader_cache,
        &mut budget(2),
    )
    .await
    .unwrap();
    assert_eq!(reader_records.len(), 1);
    assert_eq!(reader_records[0].status, QaStatus::Pending);
    assert_eq!(reader_records[0].label, None);
    assert_eq!(reader_records[0].hypothesis, None);

    let mut judge_cache = JsonQaCache::open(temp.path().join("judge")).unwrap();
    let mut judge_failure = FakeBackend::scripted([
        Ok(response("Take the train", 100, 8)),
        Err(anyhow!("HTTP 503")),
    ]);
    let judge_records = evaluate_longmem(
        &dataset,
        &rankings,
        &mut judge_failure,
        &mut judge_cache,
        &mut budget(2),
    )
    .await
    .unwrap();
    assert_eq!(judge_records.len(), 1);
    assert_eq!(judge_records[0].status, QaStatus::Pending);
    assert_eq!(judge_records[0].label, None);
    assert_eq!(
        judge_records[0].hypothesis.as_deref(),
        Some("Take the train")
    );
    assert!(judge_records[0]
        .pending_reason
        .as_deref()
        .unwrap()
        .contains("HTTP 503"));
}

#[tokio::test]
async fn request_token_and_usd_exhaustion_are_pending_without_calls_or_labels() {
    let dataset = fixture_dataset();
    let rankings = frozen_rankings(&dataset);
    let temp = tempfile::tempdir().unwrap();
    let limits = [
        QaBudgetLimits {
            max_requests: 0,
            ..budget(2).limits().clone()
        },
        QaBudgetLimits {
            max_total_tokens: 0,
            ..budget(2).limits().clone()
        },
        QaBudgetLimits {
            max_usd_micros: 0,
            ..budget(2).limits().clone()
        },
    ];

    for (index, limits) in limits.into_iter().enumerate() {
        let mut cache = JsonQaCache::open(temp.path().join(index.to_string())).unwrap();
        let mut backend = FakeBackend::scripted([
            Ok(response("Take the train", 100, 8)),
            Ok(response("yes", 40, 1)),
        ]);
        let records = evaluate_longmem(
            &dataset,
            &rankings,
            &mut backend,
            &mut cache,
            &mut QaBudget::new(limits),
        )
        .await
        .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, QaStatus::Pending);
        assert_eq!(records[0].label, None);
        assert!(backend.requests.is_empty());
    }
}

#[tokio::test]
async fn one_admitted_request_produces_at_most_one_backend_transmission() {
    let dataset = fixture_dataset();
    let rankings = frozen_rankings(&dataset);
    let temp = tempfile::tempdir().unwrap();
    let mut cache = JsonQaCache::open(temp.path()).unwrap();
    let mut backend = FakeBackend::scripted([
        Ok(response("Take the train", 100, 8)),
        Ok(response("yes", 40, 1)),
    ]);
    let mut request_budget = budget(1);

    let records = evaluate_longmem(
        &dataset,
        &rankings,
        &mut backend,
        &mut cache,
        &mut request_budget,
    )
    .await
    .unwrap();

    assert_eq!(backend.requests.len(), 1);
    assert_eq!(request_budget.requests(), 1);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, QaStatus::Pending);
    assert_eq!(records[0].label, None);
}

#[tokio::test]
async fn received_usage_and_cost_survive_budget_recording_failure() {
    let dataset = fixture_dataset();
    let rankings = frozen_rankings(&dataset);
    let temp = tempfile::tempdir().unwrap();
    let mut cache = JsonQaCache::open(temp.path()).unwrap();
    let mut backend = FakeBackend::scripted([Ok(response("Take the train", 100, 8))]);
    let mut over_ceiling = QaBudget::new(QaBudgetLimits {
        max_total_tokens: 100,
        ..budget(2).limits().clone()
    });

    let records = evaluate_longmem(
        &dataset,
        &rankings,
        &mut backend,
        &mut cache,
        &mut over_ceiling,
    )
    .await
    .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, QaStatus::Pending);
    assert_eq!(records[0].label, None);
    assert_eq!(records[0].hypothesis.as_deref(), Some("Take the train"));
    assert_eq!(records[0].usage, response("unused", 100, 8).usage);
    assert_eq!(records[0].cost_usd_micros, 1);
    assert_eq!(over_ceiling.usage(), records[0].usage);
    assert_eq!(over_ceiling.cost_usd_micros(), 1);
    assert_eq!(cache.entry_count().unwrap(), 0);
}

#[tokio::test]
async fn received_usage_and_cost_survive_cache_persistence_failure() {
    let dataset = fixture_dataset();
    let rankings = frozen_rankings(&dataset);
    let mut cache = FailingPutCache;
    let mut backend = FakeBackend::scripted([Ok(response("Take the train", 100, 8))]);
    let mut paid_budget = budget(2);

    let records = evaluate_longmem(
        &dataset,
        &rankings,
        &mut backend,
        &mut cache,
        &mut paid_budget,
    )
    .await
    .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, QaStatus::Pending);
    assert_eq!(records[0].label, None);
    assert_eq!(records[0].hypothesis.as_deref(), Some("Take the train"));
    assert_eq!(records[0].usage, response("unused", 100, 8).usage);
    assert_eq!(records[0].cost_usd_micros, 1);
    assert_eq!(paid_budget.usage(), records[0].usage);
    assert_eq!(paid_budget.cost_usd_micros(), 1);
    assert!(records[0]
        .pending_reason
        .as_deref()
        .unwrap()
        .contains("injected cache put failure"));
}

#[tokio::test]
async fn malformed_fake_usage_is_pending_and_never_cached_as_a_hypothesis() {
    let dataset = fixture_dataset();
    let rankings = frozen_rankings(&dataset);
    let temp = tempfile::tempdir().unwrap();
    let mut cache = JsonQaCache::open(temp.path()).unwrap();
    let mut malformed = response("Take the train", 100, 8);
    malformed.usage.total_tokens += 1;
    let mut backend = FakeBackend::scripted([Ok(malformed)]);

    let records = evaluate_longmem(
        &dataset,
        &rankings,
        &mut backend,
        &mut cache,
        &mut budget(2),
    )
    .await
    .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, QaStatus::Pending);
    assert_eq!(records[0].label, None);
    assert_eq!(records[0].hypothesis, None);
    assert_eq!(cache.entry_count().unwrap(), 0);
}

#[test]
fn missing_api_key_is_rejected_before_the_paid_backend_can_send() {
    let backend = OpenAiResponsesBackend::new(None);
    assert!(!backend.credentials_available());
}

#[test]
fn paid_backend_debug_redacts_api_key() {
    let api_key = "sk-regression-secret";
    let backend = OpenAiResponsesBackend::new(Some(api_key.into()));

    let debug = format!("{backend:?}");

    assert!(!debug.contains(api_key), "credential leaked via Debug");
    assert!(debug.contains("credentials_available: true"));
}

#[test]
fn paid_backend_debug_exposes_single_attempt_and_finite_timeout_policy() {
    let backend = OpenAiResponsesBackend::new(None);

    let debug = format!("{backend:?}");

    assert!(debug.contains("physical_transmissions_per_call: 1"));
    assert!(debug.contains("retry_policy: \"never\""));
    assert!(debug.contains("redirect_policy: \"none\""));
    assert!(debug.contains("total_timeout: 120s"));
    assert!(debug.contains("connect_timeout: 10s"));
    assert!(debug.contains("read_timeout: 60s"));
}

#[test]
fn response_fixture_remains_json_data_not_a_network_transport() {
    // All async evaluation tests above use FakeBackend. This explicit guard
    // keeps the only adapter-facing coverage confined to pure JSON shaping.
    let value: Value = response("yes", 1, 1).raw_response;
    assert_eq!(value["status"], "completed");
}
