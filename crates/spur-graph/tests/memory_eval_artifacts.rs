use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use sha2::{Digest, Sha256};
use spur_graph::memory_eval::{
    artifacts::{
        ArtifactDigest, ArtifactWriter, MetricValue, QaArtifactKind, QaProgress, ReleaseGates,
        RetrievalGateEvidence, RetrievalMetrics, RunEvent, RunManifest, RunState,
    },
    contract::{Cohorts, ContractId, DatasetKind, SourcePin, ValidationFinding, ValidationReport},
    ranking::{Granularity, QueryOccurrenceId, RankedHit, Ranking, RankingSet, Variant},
};

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[test]
fn qa_pending_cannot_publish_full() {
    let mut run = valid_run(["q1", "q2"]);
    run.transition(RunEvent::RetrievalComplete).unwrap();
    run.transition(RunEvent::QaPending).unwrap();

    let error = run.transition(RunEvent::PublishFull).unwrap_err();

    assert!(error.to_string().contains("qa_complete"));
    assert_eq!(run.state, RunState::QaPending);
    assert_eq!(run.qa_state, Some(RunState::QaPending));
}

#[test]
fn retrieval_publication_requires_every_section_4_gate_and_complete_rankings() {
    for missing in ["gold_leak_free", "denominators_valid", "metrics_finite"] {
        let report = valid_validation();
        let evidence = RetrievalGateEvidence {
            gold_leak_free: missing != "gold_leak_free",
            denominators_valid: missing != "denominators_valid",
            metrics_finite: missing != "metrics_finite",
        };
        let mut run = manifest(&report, evidence, ["q1"]);
        add_all_ranking_hashes(&mut run);
        assert!(
            run.transition(RunEvent::RetrievalComplete).is_err(),
            "missing gate {missing} was accepted"
        );
    }

    for fatal_code in [
        "source_hash_mismatch",
        "schema_mismatch",
        "duplicate_internal_id",
    ] {
        let mut report = valid_validation();
        report.fatal.push(ValidationFinding {
            code: fatal_code.to_owned(),
            message: "fatal fixture".to_owned(),
            question_id: None,
            eligibility_effect: None,
        });
        let mut run = manifest(&report, valid_evidence(), ["q1"]);
        add_all_ranking_hashes(&mut run);
        assert!(
            run.transition(RunEvent::RetrievalComplete).is_err(),
            "fatal validation {fatal_code} was accepted"
        );
    }

    let mut incomplete = manifest(&valid_validation(), valid_evidence(), ["q1"]);
    add_all_ranking_hashes(&mut incomplete);
    incomplete.ranking_hashes.remove(&Variant::Oracle);
    assert!(incomplete.transition(RunEvent::RetrievalComplete).is_err());

    let mut complete = valid_run(["q1"]);
    complete.transition(RunEvent::RetrievalComplete).unwrap();
    complete.transition(RunEvent::PublishRetrieval).unwrap();
    assert_eq!(complete.state, RunState::PublishedRetrieval);
}

#[test]
fn complete_qa_publishes_full_and_pending_progress_retains_every_denominator() {
    let mut run = valid_run(["q1", "q2", "q3"]);
    run.transition(RunEvent::RetrievalComplete).unwrap();
    run.qa_progress.mark_completed("q1").unwrap();
    run.transition(RunEvent::QaPending).unwrap();

    let resumed: RunManifest = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    assert_eq!(resumed.qa_progress.denominator(), 3);
    assert_eq!(
        resumed.qa_progress.completed_question_ids(),
        &BTreeSet::from(["q1".to_owned()])
    );

    let mut resumed = resumed;
    resumed.qa_progress.mark_completed("q2").unwrap();
    resumed.qa_progress.mark_completed("q3").unwrap();
    resumed.transition(RunEvent::QaComplete).unwrap();
    resumed.transition(RunEvent::PublishFull).unwrap();

    assert_eq!(resumed.state, RunState::PublishedFull);
    assert_eq!(resumed.qa_state, Some(RunState::QaComplete));
}

#[test]
fn published_retrieval_can_remain_resumable_as_qa_pending() {
    let mut run = valid_run(["q1", "q2"]);
    run.transition(RunEvent::RetrievalComplete).unwrap();
    run.transition(RunEvent::PublishRetrieval).unwrap();
    run.transition(RunEvent::QaPending).unwrap();

    assert_eq!(run.state, RunState::PublishedRetrieval);
    assert_eq!(run.qa_state, Some(RunState::QaPending));
    assert_eq!(run.qa_progress.denominator(), 2);
    assert!(run.qa_progress.completed_question_ids().is_empty());
}

#[test]
fn artifact_writer_creates_approved_layout_and_hashes_every_published_file() {
    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    let validation = valid_validation();
    let mut manifest = manifest(&validation, valid_evidence(), ["q1"]);
    let rankings = rankings(Variant::Recent, 1.0);

    writer.write_validation(&validation).unwrap();
    writer
        .write_rankings(&mut manifest, Variant::Recent, &rankings)
        .unwrap();
    writer
        .write_metrics(&manifest, &[retrieval_metrics(Variant::Recent)])
        .unwrap();
    writer
        .write_qa_json(QaArtifactKind::Prompt, "q1", &json!({"prompt": "hello"}))
        .unwrap();
    writer.write_report("# Audited report\n").unwrap();
    writer.write_manifest(&manifest).unwrap();

    for directory in [
        "rankings",
        "metrics",
        "qa/prompts",
        "qa/hypotheses",
        "qa/judge-inputs",
        "qa/labels",
    ] {
        assert!(root.path().join(directory).is_dir(), "missing {directory}");
    }
    for file in [
        "manifest.json",
        "validation.json",
        "rankings/recent.jsonl",
        "metrics/locomo-turn.json",
        "report.md",
    ] {
        assert!(root.path().join(file).is_file(), "missing {file}");
    }

    let sums = std::fs::read_to_string(root.path().join("SHA256SUMS")).unwrap();
    for required in [
        "manifest.json",
        "validation.json",
        "rankings/recent.jsonl",
        "metrics/locomo-turn.json",
        "report.md",
    ] {
        assert!(sums.contains(required), "SHA256SUMS omitted {required}");
    }
    writer.verify_checksums().unwrap();
    assert_no_temporary_files(root.path());
}

#[test]
fn checksum_verification_detects_tampering() {
    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    writer.write_report("original\n").unwrap();
    writer.verify_checksums().unwrap();

    std::fs::write(root.path().join("report.md"), "tampered\n").unwrap();

    let error = writer.verify_checksums().unwrap_err();
    assert!(error.to_string().contains("checksum mismatch"));
}

#[test]
fn checksum_reconciliation_rejects_unvalidated_or_non_cache_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    writer.write_report("original\n").unwrap();
    let bytes = br#"{"valid":"json"}"#;
    std::fs::write(root.path().join("arbitrary.json"), bytes).unwrap();

    let error = writer
        .reconcile_qa_cache_checksums(&[ArtifactDigest {
            relative_path: "arbitrary.json".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }])
        .unwrap_err();
    assert!(error.to_string().contains("recognized QA cache paths"));

    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    writer.write_report("original\n").unwrap();
    let path = root
        .path()
        .join(format!("qa/cache/locomo/{}.json", "0".repeat(64)));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();

    let error = writer.reconcile_qa_cache_checksums(&[]).unwrap_err();
    assert!(error
        .to_string()
        .contains("not a validated QA cache record"));
}

#[test]
fn recorded_ranking_hashes_are_immutable() {
    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    let validation = valid_validation();
    let mut manifest = manifest(&validation, valid_evidence(), ["q1"]);
    let original = rankings(Variant::Recent, 1.0);
    let replacement = rankings(Variant::Recent, 0.5);

    let recorded = writer
        .write_rankings(&mut manifest, Variant::Recent, &original)
        .unwrap();
    let original_bytes = std::fs::read(root.path().join("rankings/recent.jsonl")).unwrap();
    let error = writer
        .write_rankings(&mut manifest, Variant::Recent, &replacement)
        .unwrap_err();

    assert!(error.to_string().contains("immutable ranking hash"));
    assert_eq!(manifest.ranking_hashes[&Variant::Recent], recorded.sha256);
    assert_eq!(
        std::fs::read(root.path().join("rankings/recent.jsonl")).unwrap(),
        original_bytes
    );
    writer.verify_checksums().unwrap();
}

#[test]
fn ranking_jsonl_keeps_question_identity_and_both_granularities() {
    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    let validation = valid_validation();
    let mut manifest = manifest(&validation, valid_evidence(), ["q1"]);

    writer
        .write_rankings(
            &mut manifest,
            Variant::Recent,
            &rankings(Variant::Recent, 1.0),
        )
        .unwrap();

    let lines = std::fs::read_to_string(root.path().join("rankings/recent.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line["question_id"] == "q1"));
    assert_eq!(
        lines
            .iter()
            .map(|line| line["granularity"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["session", "turn"])
    );
}

#[test]
fn metric_files_bind_to_the_recorded_ranking_hash() {
    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    let validation = valid_validation();
    let mut manifest = manifest(&validation, valid_evidence(), ["q1"]);
    writer
        .write_rankings(
            &mut manifest,
            Variant::Recent,
            &rankings(Variant::Recent, 1.0),
        )
        .unwrap();

    writer
        .write_metrics(&manifest, &[retrieval_metrics(Variant::Recent)])
        .unwrap();

    let metric: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.path().join("metrics/locomo-turn.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        metric["variants"][0]["source_ranking_hash"],
        manifest.ranking_hashes[&Variant::Recent]
    );
    assert_eq!(
        metric["variants"][0]["overall"]["recall@5"]["numerator"],
        1.0
    );
    assert_eq!(
        metric["variants"][0]["overall"]["recall@5"]["denominator"],
        1
    );
    assert!(metric["variants"][0]["exclusions"].is_array());
}

#[test]
fn completed_qa_labels_are_immutable_across_resume() {
    let root = tempfile::tempdir().unwrap();
    let writer = ArtifactWriter::new(root.path()).unwrap();
    let validation = valid_validation();
    let mut manifest = manifest(&validation, valid_evidence(), ["q1", "q2"]);

    let first = writer
        .write_qa_label(&mut manifest, "q1", &json!({"label": true}))
        .unwrap();
    let error = writer
        .write_qa_label(&mut manifest, "q1", &json!({"label": false}))
        .unwrap_err();

    assert!(error.to_string().contains("immutable QA label"));
    assert_eq!(
        manifest.qa_progress.completed_question_ids(),
        &BTreeSet::from(["q1".to_owned()])
    );
    assert_eq!(
        std::fs::read(root.path().join(first.relative_path)).unwrap(),
        serde_json::to_vec_pretty(&json!({"label": true})).unwrap()
    );
    writer.verify_checksums().unwrap();
}

fn valid_run<const N: usize>(question_ids: [&str; N]) -> RunManifest {
    let report = valid_validation();
    let mut manifest = manifest(&report, valid_evidence(), question_ids);
    add_all_ranking_hashes(&mut manifest);
    manifest
}

fn manifest<const N: usize>(
    validation: &ValidationReport,
    evidence: RetrievalGateEvidence,
    question_ids: [&str; N],
) -> RunManifest {
    RunManifest::new(
        "run-2026-08-27",
        "42762436f915c6f255f3199a73cae2dfe269d7d3",
        false,
        vec![SourcePin {
            origin: "https://example.invalid/dataset.json".to_owned(),
            revision: "pinned-revision".to_owned(),
            sha256: HASH_A.to_owned(),
        }],
        ContractId::Audited("origin-faithful-v1".to_owned()),
        vec!["spur".to_owned(), "memory-eval".to_owned()],
        ReleaseGates::from_validation(validation, evidence),
        QaProgress::new(question_ids).unwrap(),
    )
}

fn valid_validation() -> ValidationReport {
    ValidationReport {
        contract_id: ContractId::Audited("origin-faithful-v1".to_owned()),
        fatal: Vec::new(),
        findings: Vec::new(),
        cohorts: Cohorts {
            locomo_retrieval: vec!["q1".to_owned()],
            locomo_qa: vec!["q1".to_owned()],
            ..Cohorts::default()
        },
    }
}

fn valid_evidence() -> RetrievalGateEvidence {
    RetrievalGateEvidence {
        gold_leak_free: true,
        denominators_valid: true,
        metrics_finite: true,
    }
}

fn add_all_ranking_hashes(manifest: &mut RunManifest) {
    for variant in [
        Variant::Oracle,
        Variant::Recent,
        Variant::FlatBm25,
        Variant::GraphIndexOnly,
        Variant::GraphTraversal,
    ] {
        manifest.ranking_hashes.insert(variant, HASH_A.to_owned());
    }
}

fn rankings(variant: Variant, score: f64) -> RankingSet {
    [Granularity::Turn, Granularity::Session]
        .into_iter()
        .map(|granularity| {
            (
                (QueryOccurrenceId::new("q1"), variant, granularity),
                Ranking {
                    variant,
                    granularity,
                    k: 5,
                    hits: vec![RankedHit {
                        occurrence_id: "occ-1".to_owned(),
                        provenance_id: None,
                        score,
                    }],
                    query_sha256: HASH_A.to_owned(),
                    corpus_sha256: HASH_A.to_owned(),
                    serialization_sha256: HASH_A.to_owned(),
                },
            )
        })
        .collect()
}

fn retrieval_metrics(variant: Variant) -> RetrievalMetrics {
    RetrievalMetrics {
        dataset: DatasetKind::Locomo,
        granularity: Granularity::Turn,
        variant,
        overall: BTreeMap::from([(
            "recall@5".to_owned(),
            MetricValue {
                value: 1.0,
                numerator: 1.0,
                denominator: 1,
            },
        )]),
        slices: BTreeMap::new(),
        exclusions: Vec::new(),
    }
}

fn assert_no_temporary_files(root: &std::path::Path) {
    fn visit(path: &std::path::Path, temporary: &mut Vec<String>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                visit(&entry.path(), temporary);
            } else if entry.file_name().to_string_lossy().contains(".tmp-") {
                temporary.push(entry.path().display().to_string());
            }
        }
    }

    let mut temporary = Vec::new();
    visit(root, &mut temporary);
    assert!(
        temporary.is_empty(),
        "temporary files remained: {temporary:?}"
    );
}
