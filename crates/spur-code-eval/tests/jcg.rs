use std::{
    future::Future,
    path::Path,
    pin::pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};

pub use spur_code_eval::{
    retrieve, BackendCall, BackendResponse, CaseStatus, CodeEvalCase, ContentPin, ContractError,
    GoldCallEdge, GoldEvidence, Language, LeakageKind, LeakagePolicy, QueryBackend,
    QueryBackendFuture, QueryError, QueryPolicy, RepositoryPin, RetrievalRequest, SourceFormat,
    SourceIdentity, SourceKind, SourceSpec, Suite,
};

#[path = "../src/jcg.rs"]
mod jcg;

use jcg::{
    ExpectationKind, ExpectationResult, FrozenCallSiteEvidence, JcgAdapter, JcgAuditStatus,
    JcgRecord,
};
use serde::Deserialize;
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("fixtures/jcg.json");
const MANIFEST: &str = include_str!("../benchmarks/code_eval.toml");

#[derive(Deserialize)]
struct FixtureCase {
    record: JcgRecord,
    frozen_call_sites: Vec<FrozenCallSiteEvidence>,
}

fn fixtures() -> Vec<FixtureCase> {
    serde_json::from_str(FIXTURE).expect("JCG fixture is valid")
}

fn fixture_with_prompt(prompt: &str) -> FixtureCase {
    let mut fixtures: Value = serde_json::from_str(FIXTURE).unwrap();
    fixtures[0]["record"]["prompt"] = Value::String(prompt.to_owned());
    serde_json::from_value(fixtures[0].clone()).unwrap()
}

#[derive(Default)]
struct CountingBackend(AtomicUsize);

impl CountingBackend {
    fn call_count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl QueryBackend for CountingBackend {
    fn dispatch<'a>(
        &'a self,
        _source_root: &'a Path,
        _call: BackendCall,
    ) -> QueryBackendFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(BackendResponse::new(
                json!({"primary_evidence": []}),
                Duration::ZERO,
            ))
        })
    }
}

fn adapter() -> JcgAdapter {
    let manifest = spur_code_eval::SourceManifest::from_toml(MANIFEST).unwrap();
    let source = manifest
        .sources()
        .iter()
        .find(|source| source.suite() == Suite::Jcg)
        .unwrap();
    JcgAdapter::new(source).unwrap()
}

#[test]
fn direct_indirect_and_prohibited_expectations_have_distinct_audited_outcomes() {
    let fixture = fixtures().remove(0);
    assert_eq!(fixture.record.language(), "javascript");
    assert_eq!(fixture.record.expectations().len(), 3);
    assert_eq!(
        fixture.record.expectations()[0].kind(),
        ExpectationKind::Prohibited
    );
    assert_eq!(fixture.record.expectations()[0].caller(), "<global>");
    assert_eq!(fixture.record.expectations()[0].target(), "CO1.forbidden");

    let evaluation = adapter()
        .evaluate(&fixture.record, &fixture.frozen_call_sites)
        .unwrap();

    assert!(matches!(evaluation.case().status(), CaseStatus::Eligible));
    assert!(evaluation.case().is_denominator_visible());
    assert_eq!(evaluation.audit().normalized_call_sites().len(), 4);
    assert_eq!(
        evaluation.audit().normalized_call_sites()[0].caller_method(),
        "<global>"
    );
    assert_eq!(
        evaluation.audit().normalized_call_sites()[0].resolved_targets(),
        ["CO1.bridge"]
    );
    assert_eq!(
        evaluation.audit().normalized_call_sites()[0]
            .provenance()
            .len(),
        2,
        "duplicate call sites merge without losing frozen provenance"
    );
    let first_call_site = &evaluation.audit().normalized_call_sites()[0];
    assert_eq!(first_call_site.source_path(), "co/CO1.js");
    assert_eq!(first_call_site.call_site_line(), 18);
    assert_eq!(first_call_site.declared_target(), "invoke");
    assert!(first_call_site.unresolved_reasons().is_empty());
    let first_provenance = &first_call_site.provenance()[0];
    assert_eq!(first_provenance.source().path(), "co/CO1.js");
    assert_eq!(
        first_provenance.source_kinds(),
        [SourceKind::SemanticKnowledgePack]
    );
    assert_eq!(first_provenance.retrieval_rank(), 1);

    let outcomes = evaluation.audit().expectation_outcomes();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0].kind(), ExpectationKind::Direct);
    assert_eq!(outcomes[0].result(), ExpectationResult::Matched);
    assert_eq!(outcomes[0].witness().len(), 1);
    assert_eq!(outcomes[1].kind(), ExpectationKind::Indirect);
    assert_eq!(outcomes[1].result(), ExpectationResult::Matched);
    assert_eq!(outcomes[1].witness().len(), 2);
    assert_eq!(outcomes[2].kind(), ExpectationKind::Prohibited);
    assert_eq!(outcomes[2].result(), ExpectationResult::Violated);
    assert!(outcomes[2]
        .diagnostic()
        .is_some_and(|diagnostic| diagnostic.contains("CO1.forbidden")));

    let recall = evaluation
        .audit()
        .annotated_positive_recall()
        .expect("eligible positive annotations have scoped recall");
    assert_eq!((recall.matched(), recall.expected()), (2, 2));
    assert_eq!(evaluation.audit().prohibited_summary().expected(), 1);
    assert_eq!(evaluation.audit().prohibited_summary().violated(), 1);
    assert!(matches!(
        evaluation.audit().status(),
        JcgAuditStatus::Partial {
            unresolved_call_sites: 1
        }
    ));
    assert_eq!(evaluation.audit().unresolved_call_sites().len(), 1);

    let audit_json = serde_json::to_string(evaluation.audit()).unwrap();
    assert!(audit_json.contains("annotated_positive_recall"));
    assert!(!audit_json.contains("precision"));
    assert!(outcomes.iter().all(|outcome| outcome
        .diagnostic()
        .is_none_or(|diagnostic| !diagnostic.contains("CO1.extra"))));
}

#[test]
fn supported_languages_score_while_unsupported_java_stays_denominator_visible() {
    let mut fixtures = fixtures();
    let javascript = fixtures.remove(0);
    let python = fixtures.remove(0);
    let java = fixtures.remove(0);

    let javascript = adapter()
        .evaluate(&javascript.record, &javascript.frozen_call_sites)
        .unwrap();
    let python = adapter()
        .evaluate(&python.record, &python.frozen_call_sites)
        .unwrap();
    let java = adapter()
        .evaluate(&java.record, &java.frozen_call_sites)
        .unwrap();

    assert!(matches!(javascript.case().status(), CaseStatus::Eligible));
    assert!(matches!(python.case().status(), CaseStatus::Eligible));
    assert_eq!(
        python
            .audit()
            .annotated_positive_recall()
            .map(|recall| (recall.matched(), recall.expected())),
        Some((1, 1))
    );
    assert_eq!(python.audit().prohibited_summary().violated(), 0);
    assert!(matches!(
        java.case().status(),
        CaseStatus::Unsupported { .. }
    ));
    assert!(java.case().is_denominator_visible());
    assert!(java
        .case()
        .status()
        .reason()
        .is_some_and(|reason| { reason == "Java extraction is outside the first full JCG lane" }));
    assert!(java.audit().annotated_positive_recall().is_none());
    assert!(java.audit().expectation_outcomes().is_empty());
    assert!(matches!(
        java.audit().status(),
        JcgAuditStatus::Unsupported { .. }
    ));
}

#[test]
fn canonical_order_is_independent_of_frozen_input_order_and_duplicates() {
    let fixture = fixtures().remove(0);
    let forward = adapter()
        .evaluate(&fixture.record, &fixture.frozen_call_sites)
        .unwrap();
    let mut reversed = fixture.frozen_call_sites;
    reversed.reverse();
    reversed.push(reversed[0].clone());
    let reverse = adapter().evaluate(&fixture.record, &reversed).unwrap();

    assert_eq!(
        serde_json::to_value(forward.audit()).unwrap(),
        serde_json::to_value(reverse.audit()).unwrap()
    );
}

#[test]
fn shared_leakage_guard_rejects_gold_before_dispatch() {
    let fixture = fixture_with_prompt("Find the direct CO1.bridge target");
    let root = std::env::temp_dir().join(format!("spur-code-eval-jcg-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let backend = CountingBackend::default();
    let request = JcgAdapter::retrieval_request(&fixture.record, &root, 10, 3).unwrap();

    let error = block_on(retrieve(&backend, &request)).unwrap_err();

    assert!(matches!(
        error,
        QueryError::ForbiddenLeakage {
            kind: LeakageKind::TargetName,
            ..
        }
    ));
    assert_eq!(backend.call_count(), 0);
    std::fs::remove_dir_all(root).unwrap();
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
