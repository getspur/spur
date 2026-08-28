use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use serde_json::json;
use spur_code_eval::{
    retrieve, AnswerStatus, BackendCall, BackendResponse, EvidenceIssueKind, GoldCallEdge,
    LeakageKind, LeakagePolicy, QueryBackend, QueryBackendFuture, QueryError, RetrievalRequest,
    SourceKind, Staleness,
};

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

#[test]
fn target_name_leakage_is_rejected_before_backend_dispatch() {
    let backend = RecordingBackend::default();
    let leakage = LeakagePolicy::new(vec!["secret_target".to_owned()], Vec::new(), Vec::new())
        .expect("valid leakage policy");
    let request = RetrievalRequest::new(
        ".",
        "explain how secret_target handles failures",
        "secret_target",
        5,
        3,
        leakage,
    )
    .expect("valid retrieval request");

    let error = block_on(retrieve(&backend, &request)).expect_err("target name must be rejected");

    assert!(matches!(
        error,
        QueryError::ForbiddenLeakage {
            kind: LeakageKind::TargetName,
            ..
        }
    ));
    assert_eq!(backend.call_count(), 0);
}

#[test]
fn hidden_completion_leakage_is_rejected_before_backend_dispatch() {
    let backend = RecordingBackend::default();
    let leakage = LeakagePolicy::new(
        Vec::new(),
        vec!["return cached_value".to_owned()],
        Vec::new(),
    )
    .expect("valid leakage policy");
    let request = RetrievalRequest::new(
        ".",
        "explain why we return cached_value after lookup",
        "lookup",
        5,
        3,
        leakage,
    )
    .expect("valid retrieval request");

    let error =
        block_on(retrieve(&backend, &request)).expect_err("hidden completion must be rejected");

    assert!(matches!(
        error,
        QueryError::ForbiddenLeakage {
            kind: LeakageKind::HiddenCompletion,
            ..
        }
    ));
    assert_eq!(backend.call_count(), 0);
}

#[test]
fn gold_call_edge_leakage_is_rejected_before_backend_dispatch() {
    let backend = RecordingBackend::default();
    let edge = GoldCallEdge::new("request_handler", "write_secret").expect("valid gold call edge");
    let leakage =
        LeakagePolicy::new(Vec::new(), Vec::new(), vec![edge]).expect("valid leakage policy");
    let request = RetrievalRequest::new(
        ".",
        "does request_handler call write_secret on failure",
        "request_handler",
        5,
        3,
        leakage,
    )
    .expect("valid retrieval request");

    let error = block_on(retrieve(&backend, &request)).expect_err("gold edge must be rejected");

    assert!(matches!(
        error,
        QueryError::ForbiddenLeakage {
            kind: LeakageKind::GoldCallEdge,
            ..
        }
    ));
    assert_eq!(backend.call_count(), 0);
}

#[test]
fn reversed_gold_call_edge_leakage_is_rejected_before_backend_dispatch() {
    let backend = RecordingBackend::default();
    let edge = GoldCallEdge::new("request_handler", "write_secret").expect("valid gold call edge");
    let leakage =
        LeakagePolicy::new(Vec::new(), Vec::new(), vec![edge]).expect("valid leakage policy");
    let request = RetrievalRequest::new(
        ".",
        "is write_secret called by request_handler on failure",
        "request_handler",
        5,
        3,
        leakage,
    )
    .expect("valid retrieval request");

    let error = block_on(retrieve(&backend, &request)).expect_err("gold edge must be rejected");

    assert!(matches!(
        error,
        QueryError::ForbiddenLeakage {
            kind: LeakageKind::GoldCallEdge,
            ..
        }
    ));
    assert_eq!(backend.call_count(), 0);
}

#[test]
fn safe_request_uses_compact_solved_dispatch_plan_under_one_root() {
    let root = TestRoot::new("dispatch-plan", "pub fn lookup() {}\n");
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(
            json!({
                "primary_evidence": [{
                    "file": "src/lib.rs",
                    "stable_symbol_id": "graph://symbol/semantic",
                    "score": 0.9,
                    "line_range": [1, 1]
                }],
                "recommended_next_tools": [{
                    "tool": "code_read_symbol",
                    "selector": "graph://symbol/semantic",
                    "project": "forbidden-response-project"
                }],
                "project": "forbidden-response-project",
                "staleness": {"analyst_matches_exact_graph": true}
            }),
            Duration::from_millis(2),
        ),
        BackendResponse::new(
            json!({
                "candidates": [{
                    "selector": "src/lib.rs::lookup",
                    "uri": "graph://symbol/exact",
                    "id": "exact",
                    "file_path": "src/lib.rs",
                    "line_range": [1, 1]
                }]
            }),
            Duration::from_millis(3),
        ),
        BackendResponse::new(
            symbol_read("graph://symbol/semantic", "src/lib.rs", [1, 1]),
            Duration::from_millis(1),
        ),
        BackendResponse::new(
            symbol_read("graph://symbol/exact", "src/lib.rs", [1, 1]),
            Duration::from_millis(1),
        ),
    ]);
    let leakage = LeakagePolicy::new(Vec::new(), Vec::new(), Vec::new())
        .expect("empty leakage policy is valid");
    let request =
        RetrievalRequest::new(root.path(), "find lookup behavior", "lookup", 5, 3, leakage)
            .expect("valid retrieval request");

    let result = block_on(retrieve(&backend, &request)).expect("safe request succeeds");

    assert_eq!(result.answer_status(), AnswerStatus::Answered);
    let recorded = backend.recorded();
    assert_eq!(
        recorded
            .iter()
            .map(|(_, call)| call.tool_name())
            .collect::<Vec<_>>(),
        vec![
            "knowledge_context_pack_2",
            "code_symbol_search",
            "code_read_symbol",
            "code_read_symbol",
        ]
    );
    assert!(recorded.len() <= 2 + 3, "exact follow-ups must be bounded");
    for (source_root, call) in recorded {
        assert_eq!(source_root, root.canonical_path());
        assert_eq!(call.arguments()["response_format"], "compact");
        assert!(call.arguments().get("project").is_none());
        assert!(call.arguments().get("root").is_none());
    }
}

#[test]
fn deduplicates_canonical_identity_before_applying_caller_top_k() {
    let root = TestRoot::new(
        "dedup-before-top-k",
        "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\n",
    );
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(
            json!({
                "primary_evidence": [
                    evidence("src/lib.rs", "alpha", [1, 1], 0.9),
                    evidence("src/lib.rs", "alpha", [1, 1], 0.8),
                    evidence("src/lib.rs", "beta", [2, 2], 0.7),
                    evidence("src/lib.rs", "gamma", [3, 3], 0.6)
                ],
                "staleness": {"analyst_matches_exact_graph": true}
            }),
            Duration::from_millis(4),
        ),
        BackendResponse::new(json!({"candidates": []}), Duration::from_millis(1)),
    ]);
    let request = RetrievalRequest::new(
        root.path(),
        "find public functions",
        "function",
        2,
        3,
        LeakagePolicy::new(Vec::new(), Vec::new(), Vec::new())
            .expect("empty leakage policy is valid"),
    )
    .expect("valid retrieval request");

    let result = block_on(retrieve(&backend, &request)).expect("retrieval succeeds");

    assert_eq!(result.hits().len(), 2);
    assert_eq!(result.hits()[0].identity().symbol_id(), Some("alpha"));
    assert!((result.hits()[0].score() - 0.9).abs() < f64::EPSILON);
    assert_eq!(result.hits()[1].identity().symbol_id(), Some("beta"));
}

#[test]
fn shuffled_rows_and_score_ties_have_identical_rankings() {
    let root = TestRoot::new(
        "stable-order",
        "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\n",
    );
    let first = [
        evidence("src/lib.rs", "beta", [2, 2], 0.5),
        evidence("src/lib.rs", "gamma", [3, 3], 0.9),
        evidence("src/lib.rs", "alpha", [1, 1], 0.5),
    ];
    let second = [first[2].clone(), first[0].clone(), first[1].clone()];

    let first_ids = ranked_ids(root.path(), first);
    let second_ids = ranked_ids(root.path(), second);

    assert_eq!(first_ids, vec!["gamma", "alpha", "beta"]);
    assert_eq!(second_ids, first_ids);
}

#[test]
fn duplicate_hits_merge_public_source_kinds() {
    let root = TestRoot::new("merge-sources", "pub fn alpha() {}\n");
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(
            json!({
                "primary_evidence": [evidence("src/lib.rs", "alpha", [1, 1], 0.9)]
            }),
            Duration::from_millis(2),
        ),
        BackendResponse::new(
            json!({
                "candidates": [{
                    "id": "alpha",
                    "file_path": "src/lib.rs",
                    "line_range": [1, 1]
                }],
                "ambiguous": true,
                "stale": true
            }),
            Duration::from_millis(1),
        ),
    ]);
    let request = safe_request(root.path(), 5, 3);

    let result = block_on(retrieve(&backend, &request)).expect("retrieval succeeds");

    assert_eq!(result.hits().len(), 1);
    assert_eq!(
        result.hits()[0].source_kinds(),
        &[
            SourceKind::SemanticKnowledgePack,
            SourceKind::ExactSymbolSearch,
        ]
    );
    assert!((result.hits()[0].score() - 0.9).abs() < f64::EPSILON);
    assert_eq!(result.hits()[0].latency_micros(), 2_000);
    assert!(result.hits()[0].ambiguous());
    assert_eq!(result.hits()[0].staleness(), Staleness::Stale);
}

#[test]
fn duplicate_and_shuffled_symbol_names_are_canonical() {
    let root = TestRoot::new("canonical-symbol-names", "pub fn alpha() {}\n");
    let first_rows = [
        evidence_with_name(
            "src/lib.rs",
            "graph://symbol/opaque-alpha",
            [1, 1],
            0.8,
            "title",
            " module::alpha ",
        ),
        evidence_with_name(
            "src/lib.rs",
            "graph://symbol/opaque-alpha",
            [1, 1],
            0.9,
            "entity_name",
            "alpha",
        ),
        evidence_with_name(
            "src/lib.rs",
            "graph://symbol/opaque-alpha",
            [1, 1],
            0.7,
            "title",
            "alpha",
        ),
    ];
    let second_rows = [
        first_rows[2].clone(),
        first_rows[0].clone(),
        first_rows[1].clone(),
    ];

    let first = retrieve_rows(root.path(), first_rows);
    let second = retrieve_rows(root.path(), second_rows);
    let first_json = serde_json::to_value(&first).unwrap();
    let second_json = serde_json::to_value(&second).unwrap();

    assert_eq!(
        first_json["hits"][0]["symbol_names"],
        json!(["alpha", "module::alpha"])
    );
    assert_eq!(
        second_json["hits"][0]["symbol_names"],
        first_json["hits"][0]["symbol_names"]
    );
}

#[test]
fn malformed_and_empty_symbol_names_are_ignored() {
    let root = TestRoot::new("malformed-symbol-names", "pub fn alpha() {}\n");
    let rows = [
        json!({
            "file": "src/lib.rs",
            "stable_symbol_id": "graph://symbol/opaque-alpha",
            "line_range": [1, 1],
            "score": 0.9,
            "entity_name": ["alpha"],
            "title": "bad\nname"
        }),
        json!({
            "file": "src/lib.rs",
            "stable_symbol_id": "graph://symbol/opaque-alpha",
            "line_range": [1, 1],
            "score": 0.8,
            "entity_name": null,
            "title": "   "
        }),
    ];

    let result = retrieve_rows(root.path(), rows);
    let serialized = serde_json::to_value(&result).unwrap();

    assert_eq!(serialized["hits"][0]["symbol_names"], json!([]));
    assert!(result.issues().is_empty());
}

#[test]
fn evidence_hit_deserializes_legacy_snapshots_without_symbol_names() {
    let root = TestRoot::new("legacy-symbol-names", "pub fn alpha() {}\n");
    let result = retrieve_rows(
        root.path(),
        [evidence(
            "src/lib.rs",
            "graph://symbol/opaque-alpha",
            [1, 1],
            0.9,
        )],
    );
    let mut legacy = serde_json::to_value(result).unwrap();
    legacy["hits"][0]
        .as_object_mut()
        .expect("serialized hit is an object")
        .remove("symbol_names");

    let reopened: spur_code_eval::RetrievalResult = serde_json::from_value(legacy).unwrap();
    let reserialized = serde_json::to_value(reopened).unwrap();

    assert_eq!(reserialized["hits"][0]["symbol_names"], json!([]));
}

#[test]
fn malformed_and_unsafe_evidence_remains_typed_and_denominator_visible() {
    let root = TestRoot::new("invalid-evidence", "pub fn alpha() {}\npub fn beta() {}\n");
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(
            json!({
                "primary_evidence": [
                    evidence("src/lib.rs", "alpha", [1, 1], 0.9),
                    evidence("src/lib.rs", "beta", [2, 2], f64::NAN.to_string()),
                    evidence("../secret.rs", "outside", [1, 1], 0.8),
                    {"stable_symbol_id": "missing-location", "score": 0.7}
                ],
                "staleness": {"analyst_matches_exact_graph": false}
            }),
            Duration::from_millis(4),
        ),
        BackendResponse::new(
            json!({"candidates": "not-an-array", "ambiguous": true}),
            Duration::from_millis(1),
        ),
    ]);
    let request = safe_request(root.path(), 5, 3);

    let result = block_on(retrieve(&backend, &request)).expect("invalid rows stay in result");

    assert_eq!(result.answer_status(), AnswerStatus::Partial);
    assert_eq!(result.hits().len(), 1);
    assert_eq!(result.hits()[0].answer_status(), AnswerStatus::Partial);
    let issue_kinds = result
        .issues()
        .iter()
        .map(spur_code_eval::EvidenceIssue::kind)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        issue_kinds,
        BTreeSet::from([
            EvidenceIssueKind::MalformedResponse,
            EvidenceIssueKind::NonFiniteScore,
            EvidenceIssueKind::OutOfRoot,
            EvidenceIssueKind::Unidentifiable,
        ])
    );
    assert!(result
        .score()
        .is_some_and(|score| (score - 0.9).abs() < f64::EPSILON));
    assert_eq!(result.latency_micros(), 5_000);
    assert!(result.response_bytes() > 0);
    assert_eq!(
        result.estimated_tokens(),
        result.response_bytes().div_ceil(4)
    );
    assert!(result.ambiguous());
    assert_eq!(result.staleness(), Staleness::Stale);
}

#[test]
fn canonical_matching_does_not_reject_identifier_substrings() {
    let root = TestRoot::new("substring", "pub fn concatenate() {}\n");
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(json!({"primary_evidence": []}), Duration::ZERO),
        BackendResponse::new(json!({"candidates": []}), Duration::ZERO),
    ]);
    let leakage = LeakagePolicy::new(vec!["cat".to_owned()], Vec::new(), Vec::new())
        .expect("valid leakage policy");
    let request = RetrievalRequest::new(
        root.path(),
        "explain concatenate behavior",
        "concatenate",
        5,
        3,
        leakage,
    )
    .expect("valid retrieval request");

    let result = block_on(retrieve(&backend, &request)).expect("substring is not leakage");

    assert_eq!(result.answer_status(), AnswerStatus::NoEvidence);
    assert_eq!(backend.call_count(), 2);
}

#[test]
fn semantic_identity_without_span_is_grounded_by_exact_read() {
    let root = TestRoot::new("semantic-grounding", "pub fn alpha() {}\n");
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(
            json!({
                "primary_evidence": [{
                    "file": "src/lib.rs",
                    "stable_symbol_id": "graph://symbol/alpha",
                    "score": 0.9
                }],
                "recommended_next_tools": [{
                    "tool": "code_read_symbol",
                    "selector": "graph://symbol/alpha"
                }]
            }),
            Duration::from_millis(2),
        ),
        BackendResponse::new(json!({"candidates": []}), Duration::from_millis(1)),
        BackendResponse::new(
            symbol_read("graph://symbol/alpha", "src/lib.rs", [1, 1]),
            Duration::from_millis(1),
        ),
    ]);
    let request = safe_request(root.path(), 5, 3);

    let result = block_on(retrieve(&backend, &request)).expect("retrieval succeeds");

    assert_eq!(result.hits().len(), 1);
    assert!((result.hits()[0].score() - 0.9).abs() < f64::EPSILON);
    assert_eq!(
        result.hits()[0].source_kinds(),
        &[
            SourceKind::SemanticKnowledgePack,
            SourceKind::ExactSymbolRead,
        ]
    );
    assert!(result.issues().is_empty());
}

fn ranked_ids(
    source_root: &Path,
    rows: impl IntoIterator<Item = serde_json::Value>,
) -> Vec<String> {
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(
            json!({"primary_evidence": rows.into_iter().collect::<Vec<_>>() }),
            Duration::ZERO,
        ),
        BackendResponse::new(json!({"candidates": []}), Duration::ZERO),
    ]);
    let request = safe_request(source_root, 5, 3);
    block_on(retrieve(&backend, &request))
        .expect("retrieval succeeds")
        .hits()
        .iter()
        .map(|hit| {
            hit.identity()
                .symbol_id()
                .expect("fixture symbol ID")
                .to_owned()
        })
        .collect()
}

fn retrieve_rows(
    source_root: &Path,
    rows: impl IntoIterator<Item = serde_json::Value>,
) -> spur_code_eval::RetrievalResult {
    let backend = RecordingBackend::with_responses([
        BackendResponse::new(
            json!({"primary_evidence": rows.into_iter().collect::<Vec<_>>() }),
            Duration::ZERO,
        ),
        BackendResponse::new(json!({"candidates": []}), Duration::ZERO),
    ]);
    block_on(retrieve(&backend, &safe_request(source_root, 5, 3))).expect("retrieval succeeds")
}

fn safe_request(source_root: &Path, top_k: usize, exact_followup_limit: usize) -> RetrievalRequest {
    RetrievalRequest::new(
        source_root,
        "find public functions",
        "function",
        top_k,
        exact_followup_limit,
        LeakagePolicy::new(Vec::new(), Vec::new(), Vec::new())
            .expect("empty leakage policy is valid"),
    )
    .expect("valid retrieval request")
}

fn symbol_read(symbol_id: &str, file_path: &str, line_range: [u64; 2]) -> serde_json::Value {
    json!({
        "symbol": {
            "file_path": file_path,
            "uri": symbol_id,
            "id": symbol_id.trim_start_matches("graph://symbol/"),
            "line_range": line_range
        }
    })
}

fn evidence(
    file_path: &str,
    symbol_id: &str,
    line_range: [u64; 2],
    score: impl serde::Serialize,
) -> serde_json::Value {
    json!({
        "file": file_path,
        "stable_symbol_id": symbol_id,
        "line_range": line_range,
        "score": score
    })
}

fn evidence_with_name(
    file_path: &str,
    symbol_id: &str,
    line_range: [u64; 2],
    score: impl serde::Serialize,
    name_field: &str,
    name: &str,
) -> serde_json::Value {
    let mut row = evidence(file_path, symbol_id, line_range, score);
    row.as_object_mut()
        .expect("evidence row is an object")
        .insert(name_field.to_owned(), json!(name));
    row
}

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str, source: &str) -> Self {
        static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
        let unique = NEXT_ROOT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "spur-code-eval-query-{}-{label}-{unique}",
            std::process::id()
        ));
        let source_dir = path.join("src");
        fs::create_dir_all(&source_dir).expect("create test source root");
        fs::write(source_dir.join("lib.rs"), source).expect("write test source");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn canonical_path(&self) -> PathBuf {
        fs::canonicalize(&self.path).expect("canonical test root")
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("remove test source root");
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
