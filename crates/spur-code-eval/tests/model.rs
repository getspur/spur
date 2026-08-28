#[path = "../src/model.rs"]
mod model;

use std::collections::{BTreeSet, VecDeque};

use model::{
    run_model_lane, ContextVariant, EncoderIndexUsage, FrozenContext, ModelBackend,
    ModelBackendError, ModelCaseStatus, ModelFailureReason, ModelOutput, ModelPendingReason,
    ModelRequest, ModelRunConfig, ModelUsage, RequestBudget, ZeroMemAccounting, ZeroMemOperation,
};

#[derive(Debug, Default)]
struct FakeBackend {
    scripted: VecDeque<Result<ModelOutput, ModelBackendError>>,
    calls: Vec<(String, ContextVariant, String)>,
}

impl FakeBackend {
    fn with_script(
        scripted: impl IntoIterator<Item = Result<ModelOutput, ModelBackendError>>,
    ) -> Self {
        Self {
            scripted: scripted.into_iter().collect(),
            calls: Vec::new(),
        }
    }
}

impl ModelBackend for FakeBackend {
    fn generate(&mut self, request: &ModelRequest<'_>) -> Result<ModelOutput, ModelBackendError> {
        self.calls.push((
            request.case_id().to_owned(),
            request.variant(),
            request.identity().cache_identity().to_owned(),
        ));
        self.scripted
            .pop_front()
            .expect("fake backend must have one response per expected call")
    }
}

fn config(prompt: &str) -> ModelRunConfig {
    ModelRunConfig::new(
        "fake-provider",
        "fake-model-v1",
        prompt,
        "fake-tokenizer-v1",
        7,
        RequestBudget::new(4_096, 1_024),
    )
    .unwrap()
}

fn complete(answer: &str) -> ModelOutput {
    ModelOutput::complete(answer, ModelUsage::new(1, 32, 8))
}

#[test]
fn partial_failure_preserves_frozen_checksums_and_resume_retries_only_unfinished_case() {
    let contexts = vec![
        FrozenContext::new("case-a", ContextVariant::Spur, "frozen SPUR context").unwrap(),
        FrozenContext::new(
            "case-b",
            ContextVariant::ZeroMemSeparatedKnowledgePack,
            "separately frozen knowledge-context pack",
        )
        .unwrap(),
    ];
    let checksums_before = contexts
        .iter()
        .map(|context| context.checksum().to_owned())
        .collect::<Vec<_>>();
    let mut first_backend = FakeBackend::with_script([
        Ok(complete("answer-a")),
        Err(ModelBackendError::Http("503".to_owned())),
    ]);

    let first = run_model_lane(&mut first_backend, &config("prompt-v1"), &contexts, &[]);

    assert!(matches!(first[0].status(), ModelCaseStatus::Completed));
    assert!(matches!(
        first[1].status(),
        ModelCaseStatus::ModelFailed(ModelFailureReason::HttpFailure)
    ));
    assert_eq!(
        contexts
            .iter()
            .map(|context| context.checksum().to_owned())
            .collect::<Vec<_>>(),
        checksums_before
    );

    let mut resume_backend = FakeBackend::with_script([Ok(complete("answer-b"))]);
    let resumed = run_model_lane(&mut resume_backend, &config("prompt-v1"), &contexts, &first);

    assert_eq!(
        resume_backend
            .calls
            .iter()
            .map(|(case_id, _, _)| case_id.as_str())
            .collect::<Vec<_>>(),
        ["case-b"]
    );
    assert_eq!(resumed[0], first[0]);
    assert!(resumed
        .iter()
        .all(|record| matches!(record.status(), ModelCaseStatus::Completed)));
    assert_eq!(
        contexts
            .iter()
            .map(|context| context.checksum().to_owned())
            .collect::<Vec<_>>(),
        checksums_before
    );
}

#[test]
fn advisory_failures_are_typed_without_a_live_backend_default() {
    let contexts = [
        FrozenContext::new("missing-credentials", ContextVariant::NoContext, "").unwrap(),
        FrozenContext::new("budget", ContextVariant::LexicalBm25, "lexical").unwrap(),
        FrozenContext::new("backend", ContextVariant::Spur, "spur").unwrap(),
        FrozenContext::new("incomplete", ContextVariant::Oracle, "oracle").unwrap(),
    ];
    let mut backend = FakeBackend::with_script([
        Err(ModelBackendError::MissingCredentials),
        Err(ModelBackendError::BudgetExhausted),
        Err(ModelBackendError::Backend("offline".to_owned())),
        Ok(ModelOutput::incomplete("partial", ModelUsage::new(1, 9, 2))),
    ]);

    let records = run_model_lane(&mut backend, &config("prompt-v1"), &contexts, &[]);

    assert!(matches!(
        records[0].status(),
        ModelCaseStatus::ModelPending(ModelPendingReason::MissingCredentials)
    ));
    assert!(matches!(
        records[1].status(),
        ModelCaseStatus::ModelPending(ModelPendingReason::BudgetExhausted)
    ));
    assert!(matches!(
        records[2].status(),
        ModelCaseStatus::ModelFailed(ModelFailureReason::BackendFailure)
    ));
    assert!(matches!(
        records[3].status(),
        ModelCaseStatus::ModelFailed(ModelFailureReason::IncompleteOutput)
    ));
}

#[test]
fn request_and_cache_identity_pin_every_variant_and_request_field() {
    let variants = [
        ContextVariant::NoContext,
        ContextVariant::LexicalBm25,
        ContextVariant::Spur,
        ContextVariant::ZeroMemSeparatedKnowledgePack,
        ContextVariant::Oracle,
    ];
    let contexts =
        variants.map(|variant| FrozenContext::new("same-case", variant, "same bytes").unwrap());
    let mut backend = FakeBackend::with_script(variants.map(|variant| {
        Ok(complete(match variant {
            ContextVariant::NoContext => "no-context",
            ContextVariant::LexicalBm25 => "lexical",
            ContextVariant::Spur => "spur",
            ContextVariant::ZeroMemSeparatedKnowledgePack => "zero-mem",
            ContextVariant::Oracle => "oracle",
        }))
    }));
    let config = config("exact pinned prompt");

    let records = run_model_lane(&mut backend, &config, &contexts, &[]);

    let cache_identities = records
        .iter()
        .map(|record| record.identity().cache_identity())
        .collect::<BTreeSet<_>>();
    assert_eq!(cache_identities.len(), variants.len());
    for (record, variant) in records.iter().zip(variants) {
        let identity = record.identity();
        assert_eq!(identity.provider(), "fake-provider");
        assert_eq!(identity.model(), "fake-model-v1");
        assert_eq!(identity.prompt(), "exact pinned prompt");
        assert_eq!(identity.tokenizer(), "fake-tokenizer-v1");
        assert_eq!(identity.seed(), 7);
        assert_eq!(identity.request_budget(), RequestBudget::new(4_096, 1_024));
        assert_eq!(identity.context().variant(), variant);
        assert_eq!(identity.request_checksum().len(), 64);
        assert_eq!(identity.cache_identity().len(), 64);
    }
}

#[test]
fn completed_record_with_changed_pins_is_failed_without_retry() {
    let contexts = [FrozenContext::new("case-a", ContextVariant::Spur, "context").unwrap()];
    let mut first_backend = FakeBackend::with_script([Ok(complete("answer"))]);
    let complete_records = run_model_lane(&mut first_backend, &config("prompt-v1"), &contexts, &[]);
    let mut resume_backend = FakeBackend::with_script([]);

    let resumed = run_model_lane(
        &mut resume_backend,
        &config("prompt-v2"),
        &contexts,
        &complete_records,
    );

    assert!(resume_backend.calls.is_empty());
    assert!(matches!(
        resumed[0].status(),
        ModelCaseStatus::ModelFailed(ModelFailureReason::IdentityMismatch)
    ));
}

#[test]
fn zero_mem_memory_operations_have_zero_llm_usage_and_separate_encoder_index_counters() {
    let mut accounting = ZeroMemAccounting::default();
    for operation in [
        ZeroMemOperation::Capture,
        ZeroMemOperation::Index,
        ZeroMemOperation::Retrieve,
        ZeroMemOperation::Update,
        ZeroMemOperation::Delete,
    ] {
        accounting.record_memory_operation(operation, EncoderIndexUsage::new(1, 10, 2, 3));
    }

    assert_eq!(accounting.memory_records().len(), 5);
    assert!(accounting
        .memory_records()
        .iter()
        .all(|record| record.llm_usage() == ModelUsage::ZERO));
    assert_eq!(
        accounting.total_encoder_index_usage(),
        EncoderIndexUsage::new(5, 50, 10, 15)
    );

    let contexts = [FrozenContext::new(
        "zero-mem-final-answer",
        ContextVariant::ZeroMemSeparatedKnowledgePack,
        "separately frozen pack",
    )
    .unwrap()];
    let mut backend = FakeBackend::with_script([Ok(complete("final answer"))]);
    let records = run_model_lane(&mut backend, &config("prompt-v1"), &contexts, &[]);

    assert_eq!(backend.calls.len(), 1);
    assert_eq!(records[0].usage(), ModelUsage::new(1, 32, 8));
    assert!(accounting
        .memory_records()
        .iter()
        .all(|record| record.llm_usage() == ModelUsage::ZERO));
}
