use std::{
    collections::VecDeque,
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::pin,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

pub use spur_code_eval::{
    retrieve, BackendCall, BackendResponse, CaseStatus, CodeEvalCase, ContentPin, ContractError,
    GoldEvidence, Language, LeakageKind, LeakagePolicy, QueryBackend, QueryBackendFuture,
    QueryError, QueryPolicy, RepositoryPin, RetrievalRequest, RetrievalResult, SourceFormat,
    SourceIdentity, SourceKind, SourceSpec, Suite,
};

#[path = "../src/crosscodeeval.rs"]
mod crosscodeeval;

use crosscodeeval::{
    derive_evidence_after_retrieval, CrossCodeAdapter, CrossCodeRecord, CrossCodeScoringInput,
    SPUR_DERIVED_EVIDENCE_VERSION,
};
use serde_json::{json, Value};

const FIXTURE: &[u8] = include_bytes!("fixtures/crosscodeeval.json");
const MANIFEST: &str = include_str!("../benchmarks/code_eval.toml");

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct RecordingBackend {
    calls: AtomicUsize,
    recorded: Mutex<Vec<(PathBuf, BackendCall)>>,
    responses: Mutex<VecDeque<BackendResponse>>,
}

impl RecordingBackend {
    fn with_responses(responses: impl IntoIterator<Item = BackendResponse>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            recorded: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn recorded(&self) -> Vec<(PathBuf, BackendCall)> {
        self.recorded.lock().expect("recorded calls lock").clone()
    }
}

impl QueryBackend for RecordingBackend {
    fn dispatch<'a>(&'a self, source_root: &'a Path, call: BackendCall) -> QueryBackendFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.recorded
            .lock()
            .expect("recorded calls lock")
            .push((source_root.to_path_buf(), call));
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .unwrap_or_else(|| {
                BackendResponse::new(json!({"primary_evidence": []}), Duration::ZERO)
            });
        Box::pin(async move { Ok(response) })
    }
}

struct FixtureRepository {
    root: PathBuf,
}

impl FixtureRepository {
    fn new() -> Self {
        let unique = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "spur-code-eval-crosscodeeval-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/current.ts"),
            "export function renderWidget() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/helper-one.ts"),
            "export function helper_one() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/helper-two.ts"),
            "export function helper_two() {}\n",
        )
        .unwrap();
        fs::write(root.join("src/Current.java"), "class Current {}\n").unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for FixtureRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn adapter() -> CrossCodeAdapter {
    let manifest = spur_code_eval::SourceManifest::from_toml(MANIFEST).unwrap();
    let source = manifest
        .sources()
        .iter()
        .find(|source| source.suite() == Suite::CrossCodeEval)
        .unwrap();
    CrossCodeAdapter::new(source).unwrap()
}

fn fixture_values() -> Vec<Value> {
    serde_json::from_slice(FIXTURE).unwrap()
}

fn fixture_record(index: usize) -> CrossCodeRecord {
    serde_json::from_value(fixture_values().remove(index)).unwrap()
}

fn record_with(index: usize, patch: impl FnOnce(&mut Value)) -> CrossCodeRecord {
    let mut value = fixture_values().remove(index);
    patch(&mut value);
    serde_json::from_value(value).unwrap()
}

fn evidence(path: &str, symbol_id: &str, score: f64) -> Value {
    json!({
        "file": path,
        "stable_symbol_id": format!("graph://symbol/{symbol_id}"),
        "score": score,
        "line_range": [1, 1]
    })
}

fn resolved_backend(rows: impl IntoIterator<Item = Value>) -> RecordingBackend {
    let rows = rows.into_iter().collect::<Vec<_>>();
    RecordingBackend::with_responses([
        BackendResponse::new(
            json!({
                "primary_evidence": rows,
                "staleness": {"analyst_matches_exact_graph": true}
            }),
            Duration::from_millis(2),
        ),
        BackendResponse::new(json!({"candidates": []}), Duration::from_millis(1)),
    ])
}

fn empty_backend() -> RecordingBackend {
    resolved_backend(std::iter::empty())
}

#[test]
fn prompt_only_retrieval_then_resolves_two_cross_file_spans_with_complete_trace() {
    let repository = FixtureRepository::new();
    let record = fixture_record(0);
    let backend = resolved_backend(vec![
        evidence("src/helper-two.ts", "helper_two", 0.8),
        evidence("src/helper-one.ts", "helper_one", 0.9),
    ]);

    let frozen = block_on(adapter().retrieval_case(&backend, &record, repository.path(), 10, 3))
        .expect("prompt-only retrieval succeeds");

    assert_eq!(frozen.query_policy().input(), record.prompt());
    let serialized_policy = serde_json::to_string(frozen.query_policy()).unwrap();
    assert!(!serialized_policy.contains(record.groundtruth()));
    for identifier in ["helper_one", "helper_two", "missing_helper"] {
        assert!(!serialized_policy.contains(identifier));
    }
    assert!(frozen.retrieval_result().is_some());
    assert_eq!(record.metadata().repository(), "example/repo");
    assert_eq!(backend.call_count(), 2);
    for (source_root, call) in backend.recorded() {
        assert_eq!(source_root, fs::canonicalize(repository.path()).unwrap());
        let arguments = serde_json::to_string(call.arguments()).unwrap();
        assert_eq!(call.arguments()["query"], record.prompt());
        assert!(!arguments.contains(record.groundtruth()));
        for identifier in ["helper_one", "helper_two", "missing_helper"] {
            assert!(!arguments.contains(identifier));
        }
    }

    // Pinned CrossCodeEval has `groundtruth`, not an identifier array. Its
    // identifiers are intentionally derived only after retrieval is frozen.
    let scoring = record.scoring_input();
    let translation = derive_evidence_after_retrieval(frozen, scoring).unwrap();
    let audit = translation.audit();

    assert_eq!(audit.resolver_version(), SPUR_DERIVED_EVIDENCE_VERSION);
    assert_eq!(audit.positive_spans().len(), 2);
    assert_eq!(
        audit
            .positive_spans()
            .iter()
            .map(SourceIdentity::path)
            .collect::<Vec<_>>(),
        vec!["src/helper-one.ts", "src/helper-two.ts"]
    );
    assert_eq!(audit.unresolved_identifiers(), ["missing_helper"]);
    assert_eq!(audit.resolution_trace().len(), 3);
    assert_eq!(audit.resolution_trace()[0].identifier(), "helper_one");
    assert!(audit.resolution_trace().iter().all(|trace| {
        trace.resolver_version() == SPUR_DERIVED_EVIDENCE_VERSION
            && (!trace.matches().is_empty() || trace.unresolved_reason().is_some())
    }));
    assert_eq!(
        audit.resolution_trace()[0].matches()[0].rank(),
        1,
        "winning source/rank metadata is retained"
    );
    let first_match = &audit.resolution_trace()[0].matches()[0];
    assert_eq!(first_match.source().path(), "src/helper-one.ts");
    assert!((first_match.score() - 0.9).abs() < f64::EPSILON);
    assert_eq!(
        first_match.source_kinds(),
        [SourceKind::SemanticKnowledgePack]
    );
    assert!(matches!(translation.case().status(), CaseStatus::Eligible));
    assert_eq!(translation.case().gold_evidence().sources().len(), 2);
    assert_eq!(
        translation.case().raw_upstream()["upstream_unknown"]["retained"],
        true
    );
    assert_eq!(
        translation.case().raw_upstream()["metadata"]["upstream_partition"],
        "line_completion"
    );
}

#[test]
fn hidden_completion_leakage_is_invalid_with_zero_backend_calls() {
    let repository = FixtureRepository::new();
    let record = record_with(0, |value| {
        value["prompt"] = json!("export function leaked() { return 7; }");
        value["groundtruth"] = json!("return 7;");
    });
    let backend = RecordingBackend::default();

    let frozen = block_on(adapter().retrieval_case(&backend, &record, repository.path(), 10, 3))
        .expect("leakage becomes a denominator-visible case");

    assert_eq!(backend.call_count(), 0);
    assert!(!frozen.query_policy().input().contains(record.groundtruth()));
    let translation = derive_evidence_after_retrieval(frozen, record.scoring_input()).unwrap();
    assert!(matches!(
        translation.case().status(),
        CaseStatus::Invalid { .. }
    ));
    assert!(translation
        .audit()
        .outcome_reason()
        .is_some_and(|reason| reason.contains("hidden completion")));
}

#[test]
fn target_identifier_leakage_is_invalid_with_zero_backend_calls() {
    let repository = FixtureRepository::new();
    let record = record_with(0, |value| {
        value["prompt"] = json!("export function leaked() { helper_secret");
        value["groundtruth"] = json!("helper_secret();");
    });
    let backend = RecordingBackend::default();

    let frozen = block_on(adapter().retrieval_case(&backend, &record, repository.path(), 10, 3))
        .expect("leakage becomes a denominator-visible case");

    assert_eq!(backend.call_count(), 0);
    let translation = derive_evidence_after_retrieval(frozen, record.scoring_input()).unwrap();
    assert!(matches!(
        translation.case().status(),
        CaseStatus::Invalid { .. }
    ));
    assert!(translation
        .audit()
        .outcome_reason()
        .is_some_and(|reason| reason.contains("target identifier")));
}

#[test]
fn duplicate_and_shuffled_evidence_produces_identical_canonical_audits() {
    let repository = FixtureRepository::new();
    let record = fixture_record(0);
    let first = resolved_backend(vec![
        evidence("src/helper-two.ts", "helper_two", 0.8),
        evidence("src/helper-one.ts", "helper_one", 0.7),
        evidence("src/helper-one.ts", "helper_one", 0.9),
    ]);
    let second = resolved_backend(vec![
        evidence("src/helper-one.ts", "helper_one", 0.9),
        evidence("src/helper-two.ts", "helper_two", 0.8),
        evidence("src/helper-one.ts", "helper_one", 0.7),
    ]);

    let first_frozen =
        block_on(adapter().retrieval_case(&first, &record, repository.path(), 10, 3)).unwrap();
    let second_frozen =
        block_on(adapter().retrieval_case(&second, &record, repository.path(), 10, 3)).unwrap();
    let scoring = record.scoring_input();
    let first_translation = derive_evidence_after_retrieval(first_frozen, scoring.clone()).unwrap();
    let second_translation = derive_evidence_after_retrieval(second_frozen, scoring).unwrap();

    assert_eq!(first_translation.audit(), second_translation.audit());
    assert_eq!(
        serde_json::to_vec(first_translation.audit()).unwrap(),
        serde_json::to_vec(second_translation.audit()).unwrap()
    );
}

#[test]
fn supported_case_without_positive_evidence_is_invalid_and_auditable() {
    let repository = FixtureRepository::new();
    let record = fixture_record(0);
    let backend = empty_backend();
    let frozen =
        block_on(adapter().retrieval_case(&backend, &record, repository.path(), 10, 3)).unwrap();

    let translation = derive_evidence_after_retrieval(frozen, record.scoring_input()).unwrap();

    assert!(matches!(
        translation.case().status(),
        CaseStatus::Invalid { .. }
    ));
    assert!(translation.case().gold_evidence().sources().is_empty());
    assert!(!translation
        .case()
        .gold_evidence()
        .derived_identifiers()
        .is_empty());
    assert!(!translation.audit().unresolved_identifiers().is_empty());
    assert!(translation.audit().outcome_reason().is_some());
}

#[test]
fn unsupported_language_without_extractor_remains_visible_without_dispatch() {
    let repository = FixtureRepository::new();
    let record = fixture_record(1);
    let backend = RecordingBackend::default();
    let frozen =
        block_on(adapter().retrieval_case(&backend, &record, repository.path(), 10, 3)).unwrap();

    assert_eq!(backend.call_count(), 0);
    assert!(frozen.retrieval_result().is_none());
    let translation = derive_evidence_after_retrieval(frozen, record.scoring_input()).unwrap();

    assert!(matches!(
        translation.case().status(),
        CaseStatus::Unsupported { .. }
    ));
    assert!(translation.case().is_denominator_visible());
    assert!(translation.audit().positive_spans().is_empty());
    assert_eq!(
        translation.audit().unresolved_identifiers(),
        ["java_helper"]
    );
    assert!(!translation.scoring_eligible());
}

#[test]
fn malformed_provenance_is_invalid_even_when_language_is_unsupported() {
    let repository = FixtureRepository::new();
    let record = record_with(1, |value| value["prompt"] = json!("  "));
    let backend = RecordingBackend::default();
    let frozen =
        block_on(adapter().retrieval_case(&backend, &record, repository.path(), 10, 3)).unwrap();

    assert_eq!(backend.call_count(), 0);
    let translation = derive_evidence_after_retrieval(frozen, record.scoring_input()).unwrap();
    assert!(matches!(
        translation.case().status(),
        CaseStatus::Invalid { .. }
    ));
}

#[test]
fn scoring_identifiers_cannot_synthesize_a_positive_absent_from_frozen_retrieval() {
    let repository = FixtureRepository::new();
    let record = fixture_record(0);
    let backend = resolved_backend(vec![evidence("src/helper-one.ts", "helper_one", 0.9)]);
    let frozen =
        block_on(adapter().retrieval_case(&backend, &record, repository.path(), 10, 3)).unwrap();
    let scoring = CrossCodeScoringInput::from_identifiers(["oracle_only"]);

    let translation = derive_evidence_after_retrieval(frozen, scoring).unwrap();

    assert!(translation.audit().positive_spans().is_empty());
    assert_eq!(
        translation.audit().unresolved_identifiers(),
        ["oracle_only"]
    );
    assert!(matches!(
        translation.case().status(),
        CaseStatus::Invalid { .. }
    ));
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
