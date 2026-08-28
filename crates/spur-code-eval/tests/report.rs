use std::collections::BTreeMap;

use serde_json::{json, Value};
use spur_code_eval::{
    content_sha256,
    metrics::{
        CrossCodeEvalMetrics, Denominators, ExactRatio, JcgMetrics, MetricSuite, RetrievalMetrics,
    },
    model::{
        run_model_lane, ContextVariant, EncoderIndexUsage, FrozenContext, ModelBackend,
        ModelBackendError, ModelOutput, ModelRequest, ModelRunConfig, ModelUsage, RequestBudget,
        ZeroMemAccounting, ZeroMemOperation,
    },
    report::{
        AdvisoryModelReport, BenchmarkReport, DeterministicReport, ReleaseInputs, ReleaseStatus,
        ReportError, ReproducibilityMetadata, SuiteReport,
    },
    ArtifactKind, ArtifactRecord, ContentPin, RepositoryPin,
};

const METRICS_PAYLOAD: &[u8] = b"{\"fixture\":\"metrics\"}\n";

fn ratio(numerator: u128, denominator: u128) -> ExactRatio {
    ExactRatio::new(numerator, denominator).unwrap()
}

fn retrieval_metrics() -> RetrievalMetrics {
    RetrievalMetrics {
        hit_at_1: ratio(1, 2),
        hit_at_5: ratio(1, 1),
        hit_at_10: ratio(1, 1),
        recall_at_1: ratio(1, 2),
        recall_at_5: ratio(1, 1),
        recall_at_10: ratio(1, 1),
        mrr: ratio(3, 4),
    }
}

fn denominators() -> Denominators {
    Denominators {
        total: 3,
        eligible: 1,
        unsupported: 1,
        invalid: 1,
        answered: 1,
        unresolved: 0,
        ambiguous: 0,
        stale: 0,
    }
}

fn deterministic_report() -> DeterministicReport {
    let repoqa = SuiteReport::new(denominators(), Some(retrieval_metrics())).unwrap();
    let crosscodeeval = SuiteReport::new(
        denominators(),
        Some(CrossCodeEvalMetrics {
            retrieval: retrieval_metrics(),
            context_coverage: ratio(3, 4),
            token_budget_precision: ratio(4, 5),
        }),
    )
    .unwrap();
    let jcg = SuiteReport::new(
        denominators(),
        Some(JcgMetrics {
            expectations_passed: 3,
            expectations_total: 4,
            expectation_pass_rate: ratio(3, 4),
            positive_targets_found: Some(2),
            positive_targets_total: Some(3),
            positive_target_recall: Some(ratio(2, 3)),
            forbidden_target_violations: 0,
        }),
    )
    .unwrap();
    DeterministicReport::new(repoqa, crosscodeeval, jcg)
}

fn artifact_record(kind: ArtifactKind, payload: &[u8]) -> ArtifactRecord {
    serde_json::from_value(json!({
        "relative_path": kind.relative_path(),
        "sha256": content_sha256(payload),
        "bytes": u64::try_from(payload.len()).unwrap(),
        "frozen": true,
    }))
    .unwrap()
}

fn reproducibility() -> ReproducibilityMetadata {
    let source_pin = ContentPin::new(
        "https://example.invalid/repoqa.tar.gz",
        "v1.2.3",
        "sha256:source",
        "MIT",
    )
    .unwrap();
    let repository_pin = RepositoryPin::new(
        "https://example.invalid/repository.git",
        "0123456789abcdef0123456789abcdef01234567",
        None,
        "sha256:materialized",
    )
    .unwrap();
    ReproducibilityMetadata {
        spur_revision: "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
        spur_dirty: false,
        platform: "x86_64-unknown-linux-gnu".to_owned(),
        command_argv: vec![
            "spur-code-eval".to_owned(),
            "report".to_owned(),
            "--run".to_owned(),
            "fixture".to_owned(),
        ],
        phase_timings_micros: BTreeMap::from([
            ("index".to_owned(), 20),
            ("retrieve".to_owned(), 30),
            ("score".to_owned(), 10),
        ]),
        peak_rss_bytes: 64 * 1024 * 1024,
        index_bytes: 4 * 1024 * 1024,
        source_pins: BTreeMap::from([("repoqa".to_owned(), source_pin)]),
        repository_pins: BTreeMap::from([("repo-a".to_owned(), repository_pin)]),
        query_policy_hash: "sha256:query-policy".to_owned(),
        scorer_versions: BTreeMap::from([
            ("crosscodeeval".to_owned(), "scorer-v1".to_owned()),
            ("jcg".to_owned(), "scorer-v1".to_owned()),
            ("repoqa".to_owned(), "scorer-v1".to_owned()),
        ]),
        adapter_versions: BTreeMap::from([
            ("crosscodeeval".to_owned(), "adapter-v1".to_owned()),
            ("jcg".to_owned(), "adapter-v1".to_owned()),
            ("repoqa".to_owned(), "adapter-v1".to_owned()),
        ]),
        suite_denominators: BTreeMap::from([
            (MetricSuite::RepoQa, denominators()),
            (MetricSuite::CrossCodeEval, denominators()),
            (MetricSuite::Jcg, denominators()),
        ]),
        artifact_records: BTreeMap::from([(
            ArtifactKind::Metrics,
            artifact_record(ArtifactKind::Metrics, METRICS_PAYLOAD),
        )]),
    }
}

fn payloads() -> BTreeMap<ArtifactKind, Vec<u8>> {
    BTreeMap::from([(ArtifactKind::Metrics, METRICS_PAYLOAD.to_vec())])
}

fn release_inputs(model_complete: bool, model_pass: bool) -> ReleaseInputs {
    ReleaseInputs::new(true, true, true, model_complete, model_pass)
}

#[test]
fn release_policy_matches_all_32_boolean_inputs() {
    for mask in 0_u8..32 {
        let bit = |offset| mask & (1_u8 << offset) != 0_u8;
        let inputs = ReleaseInputs::new(bit(4), bit(3), bit(2), bit(1), bit(0));
        let deterministic_gate = bit(4) && bit(3) && bit(2);
        let expected = if !deterministic_gate {
            ReleaseStatus::Reject
        } else if bit(1) && bit(0) {
            ReleaseStatus::PublishFull
        } else {
            ReleaseStatus::PublishDeterministic
        };

        assert_eq!(
            inputs.status(),
            expected,
            "unexpected release status for input mask {mask:05b}"
        );
    }
}

#[test]
fn deterministic_report_renders_without_a_model_and_keeps_native_suites_separate() {
    let report = BenchmarkReport::new(
        release_inputs(false, false),
        reproducibility(),
        deterministic_report(),
        None,
    )
    .unwrap();

    let first = report.render_json(&payloads()).unwrap();
    let second = report.render_json(&payloads()).unwrap();
    let json: Value = serde_json::from_slice(&first).unwrap();

    assert_eq!(first, second);
    assert_eq!(json["release_status"], "publish_deterministic");
    assert!(json["deterministic"]["repoqa"].is_object());
    assert!(json["deterministic"]["crosscodeeval"].is_object());
    assert!(json["deterministic"]["jcg"].is_object());
    assert!(json.get("headline_score").is_none());
    assert!(json.get("blended_metrics").is_none());
    assert!(json["advisory_model"].is_null());
    assert_eq!(
        json["reproducibility"]["command_argv"],
        json!(["spur-code-eval", "report", "--run", "fixture"])
    );
    assert_eq!(
        json["reproducibility"]["suite_denominators"]["repo_qa"]["eligible"],
        1
    );
    assert_eq!(
        json["reproducibility"]["artifact_records"]["metrics"]["sha256"],
        content_sha256(METRICS_PAYLOAD)
    );
}

#[test]
fn render_rejects_missing_and_mismatched_artifact_payloads() {
    let report = BenchmarkReport::new(
        release_inputs(false, false),
        reproducibility(),
        deterministic_report(),
        None,
    )
    .unwrap();

    assert!(matches!(
        report.render_json(&BTreeMap::new()),
        Err(ReportError::MissingArtifactPayload {
            kind: ArtifactKind::Metrics
        })
    ));

    let mismatched = BTreeMap::from([(ArtifactKind::Metrics, b"tampered".to_vec())]);
    assert!(matches!(
        report.render_json(&mismatched),
        Err(ReportError::ChecksumMismatch {
            kind: ArtifactKind::Metrics,
            ..
        })
    ));

    let mut unreferenced = payloads();
    unreferenced.insert(ArtifactKind::Rankings, b"unreferenced".to_vec());
    assert!(matches!(
        report.render_json(&unreferenced),
        Err(ReportError::MissingArtifactChecksum {
            kind: ArtifactKind::Rankings
        })
    ));
}

#[derive(Debug)]
struct CompleteBackend;

impl ModelBackend for CompleteBackend {
    fn generate(&mut self, _request: &ModelRequest<'_>) -> Result<ModelOutput, ModelBackendError> {
        Ok(ModelOutput::complete(
            "final answer",
            ModelUsage::new(1, 20, 5),
        ))
    }
}

#[test]
fn full_report_serializes_zero_mem_as_a_separate_advisory_model_section() {
    let context = FrozenContext::new(
        "zero-mem-case",
        ContextVariant::ZeroMemSeparatedKnowledgePack,
        "frozen separated knowledge pack",
    )
    .unwrap();
    let config = ModelRunConfig::new(
        "provider",
        "model",
        "prompt",
        "tokenizer",
        7,
        RequestBudget::new(1_024, 256),
    )
    .unwrap();
    let records = run_model_lane(&mut CompleteBackend, &config, &[context], &[]);
    let mut zero_mem = ZeroMemAccounting::default();
    zero_mem.record_memory_operation(
        ZeroMemOperation::Retrieve,
        EncoderIndexUsage::new(1, 48, 2, 0),
    );
    let advisory = AdvisoryModelReport::new(records, zero_mem).unwrap();
    let report = BenchmarkReport::new(
        release_inputs(true, true),
        reproducibility(),
        deterministic_report(),
        Some(advisory),
    )
    .unwrap();

    let bytes = report.render_json(&payloads()).unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    let advisory = &json["advisory_model"];

    assert_eq!(json["release_status"], "publish_full");
    assert_eq!(advisory["summary"]["completed"], 1);
    assert_eq!(
        advisory["records"][0]["identity"]["context"]["variant"],
        "zero_mem_separated_knowledge_pack"
    );
    assert_eq!(
        advisory["zero_mem_accounting"]["memory_records"][0]["llm_usage"],
        json!({"llm_calls": 0, "input_tokens": 0, "output_tokens": 0})
    );
    assert_eq!(
        advisory["zero_mem_accounting"]["memory_records"][0]["encoder_index_usage"],
        json!({
            "encoder_calls": 1,
            "encoder_input_tokens": 48,
            "index_reads": 2,
            "index_writes": 0
        })
    );
}

#[test]
fn construction_rejects_denominator_metadata_that_disagrees_with_native_sections() {
    let mut metadata = reproducibility();
    let repoqa_denominators = metadata
        .suite_denominators
        .get_mut(&MetricSuite::RepoQa)
        .unwrap();
    repoqa_denominators.eligible = 0;
    repoqa_denominators.unsupported = 2;
    repoqa_denominators.answered = 0;

    assert!(matches!(
        BenchmarkReport::new(
            release_inputs(false, false),
            metadata,
            deterministic_report(),
            None,
        ),
        Err(ReportError::DenominatorMismatch {
            suite: MetricSuite::RepoQa
        })
    ));
}
