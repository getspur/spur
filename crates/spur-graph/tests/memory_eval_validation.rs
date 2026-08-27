use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spur_graph::memory_eval::contract::{
    ensure_same_contract, occurrence_id, validate_dataset, BenchmarkContract, BenchmarkDataset,
    ContractId, ConversationRecord, DatasetKind, EligibilityEffect, EvidenceRef, QuestionRecord,
    Role, SessionRecord, SourcePin, TurnRecord,
};

fn source_pin(raw: &str) -> SourcePin {
    SourcePin {
        origin: "fixture://memory-eval-validation".to_owned(),
        revision: "fixture-v1".to_owned(),
        sha256: format!("{:x}", Sha256::digest(raw.as_bytes())),
    }
}

fn audited_contract() -> BenchmarkContract {
    BenchmarkContract::audited("origin-native-v1")
}

fn compatibility_contract() -> BenchmarkContract {
    BenchmarkContract::compatibility("origin-native-v1")
}

fn question(
    id: &str,
    category: Option<u32>,
    evidence: Vec<EvidenceRef>,
    raw: Value,
) -> QuestionRecord {
    QuestionRecord {
        id: id.to_owned(),
        text: format!("Question {id}"),
        question_date: None,
        answer: json!("answer"),
        category,
        question_type: None,
        evidence,
        gold_session_ids: Vec::new(),
        gold_turn_ids: Vec::new(),
        raw,
    }
}

fn locomo_dataset() -> BenchmarkDataset {
    let raw = "locomo fixture bytes";
    let turn_id = occurrence_id("locomo-turn", "conversation/session", 0, "D1:1");
    let session_id = occurrence_id("locomo-session", "conversation", 0, "session_1");
    let conversation_id = occurrence_id("locomo-conversation", "dataset", 0, "conversation");

    BenchmarkDataset::new(
        DatasetKind::Locomo,
        source_pin(raw),
        vec![ConversationRecord {
            internal_id: conversation_id,
            source_id: Some("conversation".to_owned()),
            sessions: vec![SessionRecord {
                internal_id: session_id,
                source_id: Some("session_1".to_owned()),
                occurred_at: Some("2024-01-01".to_owned()),
                turns: vec![TurnRecord {
                    internal_id: turn_id.clone(),
                    source_id: Some("D1:1".to_owned()),
                    role: Role::User,
                    speaker: Some("Alice".to_owned()),
                    content: "origin text".to_owned(),
                    caption: None,
                    has_answer: None,
                    raw: json!({"speaker": "Alice", "dia_id": "D1:1", "text": "origin text"}),
                }],
                raw: json!([{"speaker": "Alice", "dia_id": "D1:1", "text": "origin text"}]),
            }],
            raw: json!({"sample_id": "conversation", "conversation": {}, "qa": []}),
        }],
        vec![
            question(
                "q-good",
                Some(1),
                vec![EvidenceRef {
                    raw: "D1:1".to_owned(),
                    resolved_turn_id: Some(turn_id),
                }],
                json!({"question": "good", "evidence": ["D1:1"], "category": 1}),
            ),
            question(
                "q-unresolved",
                Some(4),
                vec![EvidenceRef {
                    raw: "D9:missing".to_owned(),
                    resolved_turn_id: None,
                }],
                json!({"question": "unresolved", "evidence": ["D9:missing"], "category": 4}),
            ),
            question(
                "q-evidence-free",
                Some(3),
                Vec::new(),
                json!({"question": "evidence free", "evidence": [], "category": 3}),
            ),
            question(
                "q-adversarial",
                Some(5),
                Vec::new(),
                json!({"question": "adversarial", "evidence": [], "category": 5}),
            ),
        ],
        raw,
    )
}

fn longmemeval_dataset() -> BenchmarkDataset {
    let raw = "longmemeval fixture bytes";
    let questions = ["q-ordinary", "q-abstention_abs"]
        .into_iter()
        .map(|id| {
            question(
                id,
                None,
                Vec::new(),
                json!({
                    "question_id": id,
                    "question_type": "single-session-user",
                    "question": format!("Question {id}"),
                    "answer": "answer",
                    "haystack_session_ids": [],
                    "haystack_dates": [],
                    "haystack_sessions": [],
                    "answer_session_ids": []
                }),
            )
        })
        .collect();

    BenchmarkDataset::new(
        DatasetKind::LongMemEval,
        source_pin(raw),
        Vec::new(),
        questions,
        raw,
    )
}

#[test]
fn locomo_retrieval_requires_nonempty_fully_resolved_evidence_while_qa_keeps_all_rows() {
    let dataset = locomo_dataset();
    let report = validate_dataset(&dataset, &audited_contract());

    assert!(!report.has_fatal());
    assert_eq!(report.cohorts.locomo_retrieval, vec!["q-good"]);
    assert_eq!(
        report.cohorts.locomo_qa,
        vec!["q-good", "q-unresolved", "q-evidence-free", "q-adversarial"]
    );
}

#[test]
fn known_locomo_evidence_defects_are_nonfatal_and_name_the_question_and_effect() {
    let report = validate_dataset(&locomo_dataset(), &audited_contract());

    assert!(!report.has_fatal());
    for (question_id, code) in [
        ("q-unresolved", "unresolved_evidence"),
        ("q-evidence-free", "missing_evidence"),
        ("q-adversarial", "missing_evidence"),
    ] {
        assert!(report.findings.iter().any(|finding| {
            finding.code == code
                && finding.question_id.as_deref() == Some(question_id)
                && finding.eligibility_effect == Some(EligibilityEffect::ExcludedFromRetrieval)
        }));
    }
}

#[test]
fn longmemeval_retrieval_excludes_abstention_while_qa_keeps_every_question() {
    let report = validate_dataset(&longmemeval_dataset(), &audited_contract());

    assert!(!report.has_fatal());
    assert_eq!(report.cohorts.longmemeval_retrieval, vec!["q-ordinary"]);
    assert_eq!(
        report.cohorts.longmemeval_qa,
        vec!["q-ordinary", "q-abstention_abs"]
    );
}

#[test]
fn source_hash_mismatch_is_fatal_and_suppresses_all_cohorts() {
    let mut dataset = locomo_dataset();
    dataset.source.sha256 = "0".repeat(64);

    let report = validate_dataset(&dataset, &audited_contract());

    assert!(report
        .fatal
        .iter()
        .any(|finding| finding.code == "source_hash_mismatch"));
    assert!(report.cohorts.is_empty());
}

#[test]
fn malformed_source_hash_is_fatal_even_when_digest_fields_match() {
    let mut dataset = locomo_dataset();
    dataset.source.sha256 = "fixture".to_owned();
    dataset.raw_sha256 = "fixture".to_owned();

    let report = validate_dataset(&dataset, &audited_contract());

    assert!(report
        .fatal
        .iter()
        .any(|finding| finding.code == "source_hash_mismatch"));
}

#[test]
fn canonical_schema_mismatch_is_fatal() {
    let mut dataset = locomo_dataset();
    dataset.questions[0].raw = Value::Null;

    let report = validate_dataset(&dataset, &audited_contract());

    assert!(report
        .fatal
        .iter()
        .any(|finding| finding.code == "schema_mismatch"));
}

#[test]
fn duplicate_internal_ids_are_fatal() {
    let mut dataset = locomo_dataset();
    let duplicate = dataset.conversations[0].sessions[0].turns[0].clone();
    dataset.conversations[0].sessions[0].turns.push(duplicate);

    let report = validate_dataset(&dataset, &audited_contract());

    assert!(report
        .fatal
        .iter()
        .any(|finding| finding.code == "duplicate_internal_id"));
}

#[test]
fn broken_longmemeval_parallel_arrays_are_fatal() {
    let mut dataset = longmemeval_dataset();
    dataset.questions[0].raw["haystack_session_ids"] = json!(["s0", "s1"]);
    dataset.questions[0].raw["haystack_dates"] = json!(["2024-01-01"]);
    dataset.questions[0].raw["haystack_sessions"] = json!([[]]);

    let report = validate_dataset(&dataset, &audited_contract());

    assert!(report.fatal.iter().any(|finding| {
        finding.code == "broken_parallel_arrays"
            && finding.question_id.as_deref() == Some("q-ordinary")
    }));
}

#[test]
fn validation_never_repairs_or_transforms_source_records() {
    let dataset = locomo_dataset();
    let before = dataset.clone();

    let _ = validate_dataset(&dataset, &audited_contract());

    assert_eq!(dataset, before);
}

#[test]
fn contract_ids_refuse_blended_aggregation() {
    let audited = audited_contract();
    let compatibility = compatibility_contract();

    assert!(matches!(&audited.contract_id, ContractId::Audited(_)));
    assert!(matches!(
        &compatibility.contract_id,
        ContractId::Compatibility(_)
    ));
    assert!(ensure_same_contract(&audited, &compatibility).is_err());
    assert!(ensure_same_contract(&audited, &audited).is_ok());
    assert!(ensure_same_contract(
        &BenchmarkContract::audited("origin-native-v1"),
        &BenchmarkContract::audited("origin-native-v2")
    )
    .is_err());
}
