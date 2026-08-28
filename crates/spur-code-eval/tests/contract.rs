use serde_json::json;
use spur_code_eval::{
    CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldEvidence, Language, QueryPolicy,
    RepositoryPin, SourceIdentity, Suite, CONTRACT_VERSION,
};

const DATASET_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const REPOSITORY_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn source(path: &str, byte_start: u64, byte_end: u64) -> SourceIdentity {
    SourceIdentity::new(path, byte_start, byte_end, None).unwrap()
}

fn eligible_fixture_with_raw_field(field: &str, value: u64) -> CodeEvalCase {
    let source_a = source("src/a.rs", 3, 8);
    let source_b = source("src/b.rs", 10, 20);
    let mut raw_upstream = json!({"native_id": "upstream-7"});
    raw_upstream[field] = json!(value);

    CodeEvalCase::new(
        Suite::RepoQa,
        "repoqa-7",
        Language::new("rust").unwrap(),
        ContentPin::new(
            "https://example.invalid/repoqa.jsonl",
            DATASET_REVISION,
            "sha256:dataset",
            "MIT",
        )
        .unwrap(),
        RepositoryPin::new(
            "https://example.invalid/repository.git",
            REPOSITORY_COMMIT,
            Some("crates/example".to_owned()),
            "sha256:materialized-repository",
        )
        .unwrap(),
        QueryPolicy::new("describe the parser behavior", "sha256:query-policy").unwrap(),
        GoldEvidence::new(
            vec![source_b, source_a.clone(), source_a],
            vec![
                "target-b".to_owned(),
                "target-a".to_owned(),
                "target-a".to_owned(),
            ],
        )
        .unwrap(),
        CaseStatus::eligible(),
        raw_upstream,
    )
    .unwrap()
}

#[test]
fn raw_provenance_and_status_survive_round_trip() {
    let case = eligible_fixture_with_raw_field("vendor_extension", 7);

    let encoded = serde_json::to_vec(&case).unwrap();
    let decoded: CodeEvalCase = serde_json::from_slice(&encoded).unwrap();

    assert_eq!(decoded, case);
    assert_eq!(decoded.contract_version(), CONTRACT_VERSION);
    assert_eq!(decoded.raw_upstream()["vendor_extension"], 7);
    assert!(decoded.is_denominator_visible());
    assert_eq!(
        decoded.gold_evidence().sources(),
        &[source("src/a.rs", 3, 8), source("src/b.rs", 10, 20)]
    );
    assert_eq!(
        decoded.gold_evidence().derived_identifiers(),
        ["target-a", "target-b"]
    );
}

#[test]
fn empty_case_ids_are_rejected() {
    let case = eligible_fixture_with_raw_field("vendor_extension", 7);

    let error = CodeEvalCase::new(
        case.suite(),
        " ",
        case.language().clone(),
        case.dataset_pin().clone(),
        case.repository_pin().clone(),
        case.query_policy().clone(),
        case.gold_evidence().clone(),
        case.status().clone(),
        case.raw_upstream().clone(),
    )
    .unwrap_err();

    assert_eq!(error, ContractError::EmptyField { field: "case_id" });
}

#[test]
fn empty_evidence_ids_are_rejected() {
    let error =
        GoldEvidence::new(vec![source("src/lib.rs", 0, 4)], vec![String::new()]).unwrap_err();

    assert_eq!(
        error,
        ContractError::EmptyField {
            field: "gold_evidence.derived_identifier",
        }
    );
}

#[test]
fn revision_pins_require_immutable_values() {
    let dataset_error = ContentPin::new(
        "https://example.invalid/dataset",
        "main",
        "sha256:dataset",
        "MIT",
    )
    .unwrap_err();
    let repository_error = RepositoryPin::new(
        "https://example.invalid/repository.git",
        "main",
        None,
        "sha256:repository",
    )
    .unwrap_err();

    assert!(matches!(
        dataset_error,
        ContractError::MutableRevision {
            field: "content_pin.revision",
            ..
        }
    ));
    assert!(matches!(
        repository_error,
        ContractError::MutableRevision {
            field: "repository_pin.commit_sha",
            ..
        }
    ));
}

#[test]
fn source_spans_must_be_non_empty_and_forward() {
    assert_eq!(
        SourceIdentity::new("src/lib.rs", 4, 4, None).unwrap_err(),
        ContractError::InvalidSpan {
            byte_start: 4,
            byte_end: 4,
        }
    );
    assert_eq!(
        SourceIdentity::new("src/lib.rs", 8, 4, None).unwrap_err(),
        ContractError::InvalidSpan {
            byte_start: 8,
            byte_end: 4,
        }
    );
}

#[test]
fn every_case_status_is_denominator_visible() {
    let statuses = [
        CaseStatus::eligible(),
        CaseStatus::unsupported("java extractor unavailable").unwrap(),
        CaseStatus::invalid("gold span did not resolve").unwrap(),
    ];

    assert!(statuses.iter().all(CaseStatus::is_denominator_visible));
    assert_eq!(statuses[0].reason(), None);
    assert_eq!(statuses[1].reason(), Some("java extractor unavailable"));
    assert_eq!(statuses[2].reason(), Some("gold span did not resolve"));
}

#[test]
fn non_eligible_statuses_require_a_reason() {
    assert_eq!(
        CaseStatus::unsupported(" ").unwrap_err(),
        ContractError::EmptyField {
            field: "case_status.reason",
        }
    );
    assert_eq!(
        CaseStatus::invalid("").unwrap_err(),
        ContractError::EmptyField {
            field: "case_status.reason",
        }
    );
}
