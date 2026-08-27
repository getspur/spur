use std::collections::{BTreeMap, BTreeSet};

use spur_graph::memory_eval::{
    contract::DatasetKind,
    ranking::{Granularity, RankedHit, Ranking, Variant},
};

// Task 7 intentionally owns only metrics.rs and this test. Include the private
// sibling module so its new crate-internal API can be exercised without
// widening the public module surface owned by Task 12.
#[allow(dead_code)]
mod memory_eval {
    pub use spur_graph::memory_eval::{contract, ranking};
    pub const COVERED_WEIGHT: u32 = 1000;
    pub const PARTIAL_WEIGHT: u32 = 500;

    pub mod metrics {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/memory_eval/metrics.rs"
        ));
    }
}

use memory_eval::metrics::{
    ndcg_at_k, recall_all_at_k, recall_any_at_k, score_locomo_retrieval,
    score_longmemeval_retrieval, MetricValue, RetrievalMetricInput, RetrievalMetrics,
};

fn ranking(variant: Variant, granularity: Granularity, k: usize, hits: &[(&str, f64)]) -> Ranking {
    Ranking {
        variant,
        granularity,
        k,
        hits: hits
            .iter()
            .map(|(occurrence_id, score)| RankedHit {
                occurrence_id: (*occurrence_id).to_owned(),
                provenance_id: None,
                score: *score,
            })
            .collect(),
        query_sha256: "query".to_owned(),
        corpus_sha256: "corpus".to_owned(),
        serialization_sha256: "serialization".to_owned(),
    }
}

fn input(
    question_id: &str,
    category: Option<u32>,
    question_type: Option<&str>,
    caption_evidence: bool,
    session_gold_ids: &[&str],
    turn_gold_ids: &[&str],
    ranking: Ranking,
) -> RetrievalMetricInput {
    RetrievalMetricInput {
        question_id: question_id.to_owned(),
        category,
        question_type: question_type.map(str::to_owned),
        caption_evidence,
        session_gold_ids: session_gold_ids.iter().map(|id| (*id).to_owned()).collect(),
        turn_gold_ids: turn_gold_ids.iter().map(|id| (*id).to_owned()).collect(),
        ranking,
    }
}

fn assert_metric(metric: &MetricValue, value: f64, numerator: f64, denominator: u64) {
    assert!((metric.value - value).abs() < 1e-12, "{metric:?}");
    assert!((metric.numerator - numerator).abs() < 1e-12, "{metric:?}");
    assert_eq!(metric.denominator, denominator);
}

#[test]
fn retrieval_metrics_json_roundtrip_preserves_finite_f64_bits() {
    let computed = RetrievalMetrics {
        dataset: DatasetKind::Locomo,
        granularity: Granularity::Turn,
        variant: Variant::FlatBm25,
        overall: BTreeMap::from([(
            "ndcg_any@10".to_owned(),
            MetricValue {
                value: 0.989_356_310_187_531_7,
                numerator: 0.989_356_310_187_531_7,
                denominator: 1,
            },
        )]),
        slices: BTreeMap::new(),
        exclusions: Vec::new(),
    };

    let bytes = serde_json::to_vec_pretty(&computed).expect("serialize computed metrics");
    let persisted = serde_json::from_slice(&bytes).expect("parse persisted metric JSON");
    let reloaded: RetrievalMetrics =
        serde_json::from_value(persisted).expect("reload persisted metrics");

    let computed_value = &computed.overall["ndcg_any@10"];
    let reloaded_value = &reloaded.overall["ndcg_any@10"];
    assert_eq!(
        reloaded_value.value.to_bits(),
        computed_value.value.to_bits()
    );
    assert_eq!(
        reloaded_value.numerator.to_bits(),
        computed_value.numerator.to_bits()
    );
    assert_eq!(reloaded, computed);
}

#[test]
fn ndcg_matches_longmemeval_origin_discount_offsets() {
    let metric = ndcg_at_k(&["a", "b"], &["a", "x", "b"], 3);
    assert!((metric - 0.815_464_876_785_728_8).abs() < 1e-12);

    // A duplicate ranked occurrence cannot consume relevance twice.
    assert!((ndcg_at_k(&["a", "b"], &["a", "a", "b"], 3) - metric).abs() < 1e-12);
}

#[test]
fn locomo_reports_macro_evidence_recall_all_hit_diagnostics_and_slices() {
    let cases = vec![
        input(
            "q1",
            Some(1),
            None,
            true,
            &[],
            &["a", "b"],
            ranking(
                Variant::FlatBm25,
                Granularity::Turn,
                10,
                &[("a", 3.0), ("x", 2.0), ("b", 1.0)],
            ),
        ),
        input(
            "q2",
            Some(2),
            None,
            false,
            &[],
            &["c"],
            ranking(
                Variant::FlatBm25,
                Granularity::Turn,
                10,
                &[("x", 2.0), ("c", 1.0)],
            ),
        ),
    ];

    let report = score_locomo_retrieval(&cases, vec!["q-malformed: unresolved evidence".into()])
        .expect("score LoCoMo");

    assert_eq!(report.dataset, DatasetKind::Locomo);
    assert_eq!(report.granularity, Granularity::Turn);
    assert_eq!(report.variant, Variant::FlatBm25);
    assert_eq!(report.exclusions, ["q-malformed: unresolved evidence"]);
    assert_metric(&report.overall["evidence_recall_at_1"], 0.25, 0.5, 2);
    assert_metric(&report.overall["evidence_recall_at_5"], 1.0, 2.0, 2);
    assert_metric(&report.overall["evidence_recall_at_10"], 1.0, 2.0, 2);
    assert_metric(&report.overall["all_evidence_hit_at_1"], 0.0, 0.0, 2);
    assert_metric(&report.overall["all_evidence_hit_at_5"], 1.0, 2.0, 2);
    assert_metric(&report.overall["all_evidence_hit_at_10"], 1.0, 2.0, 2);

    assert_metric(
        &report.slices["category:1"]["evidence_recall_at_1"],
        0.5,
        0.5,
        1,
    );
    assert_metric(
        &report.slices["category:2"]["evidence_recall_at_1"],
        0.0,
        0.0,
        1,
    );
    assert_metric(
        &report.slices["caption_evidence:true"]["evidence_recall_at_5"],
        1.0,
        1.0,
        1,
    );
    assert_metric(
        &report.slices["caption_evidence:false"]["evidence_recall_at_5"],
        1.0,
        1.0,
        1,
    );
}

#[test]
fn longmem_session_and_turn_gold_remain_independent_at_exact_cutoffs() {
    let session_cases = vec![
        input(
            "q1",
            None,
            Some("multi-session"),
            false,
            &["s1", "s2"],
            &["t1"],
            ranking(
                Variant::GraphTraversal,
                Granularity::Session,
                10,
                &[("s1", 3.0), ("x", 2.0), ("s2", 1.0)],
            ),
        ),
        input(
            "q2",
            None,
            Some("single-session-user"),
            false,
            &["s3"],
            &["t3"],
            ranking(
                Variant::GraphTraversal,
                Granularity::Session,
                10,
                &[("t3", 1.0)],
            ),
        ),
    ];
    let sessions = score_longmemeval_retrieval(&session_cases, Vec::new()).expect("score sessions");

    assert_eq!(sessions.dataset, DatasetKind::LongMemEval);
    assert_eq!(sessions.granularity, Granularity::Session);
    assert_metric(&sessions.overall["recall_all@5"], 0.5, 1.0, 2);
    assert_metric(&sessions.overall["recall_any@5"], 0.5, 1.0, 2);
    assert_metric(
        &sessions.overall["ndcg_any@5"],
        0.815_464_876_785_728_8 / 2.0,
        0.815_464_876_785_728_8,
        2,
    );
    assert!(sessions.overall.contains_key("recall_all@10"));
    assert!(sessions.overall.contains_key("ndcg_any@10"));
    assert!(!sessions.overall.contains_key("recall_all@50"));
    assert_metric(
        &sessions.slices["question_type:multi-session"]["recall_all@5"],
        1.0,
        1.0,
        1,
    );
    assert_metric(
        &sessions.slices["question_type:single-session-user"]["recall_any@5"],
        0.0,
        0.0,
        1,
    );

    let turn_cases = vec![
        input(
            "q1",
            None,
            Some("multi-session"),
            false,
            &["s1"],
            &["t1"],
            ranking(
                Variant::GraphTraversal,
                Granularity::Turn,
                50,
                &[("t1", 2.0)],
            ),
        ),
        input(
            "q2",
            None,
            Some("single-session-user"),
            false,
            &["s3"],
            &["t3"],
            ranking(
                Variant::GraphTraversal,
                Granularity::Turn,
                50,
                &[("s3", 1.0)],
            ),
        ),
    ];
    let turns = score_longmemeval_retrieval(&turn_cases, Vec::new()).expect("score turns");
    assert_eq!(turns.granularity, Granularity::Turn);
    assert_metric(&turns.overall["recall_all@50"], 0.5, 1.0, 2);
    assert_metric(&turns.overall["recall_any@50"], 0.5, 1.0, 2);
    assert_metric(&turns.overall["ndcg_any@50"], 0.5, 1.0, 2);
}

#[test]
fn metric_reports_reject_empty_wrong_or_non_finite_inputs() {
    assert!(MetricValue::from_scores(&[]).is_err());
    assert!(MetricValue::from_scores(&[f64::NAN]).is_err());
    assert!(MetricValue::from_scores(&[f64::INFINITY]).is_err());
    assert!(MetricValue::from_scores(&[-0.01]).is_err());
    assert!(MetricValue::from_scores(&[1.01]).is_err());

    assert!(score_locomo_retrieval(&[], Vec::new()).is_err());

    let no_turn_gold = input(
        "q-empty",
        Some(1),
        None,
        false,
        &["session-only"],
        &[],
        ranking(Variant::Recent, Granularity::Turn, 10, &[]),
    );
    assert!(score_locomo_retrieval(&[no_turn_gold], Vec::new()).is_err());

    let no_session_gold = input(
        "q-empty",
        None,
        Some("single-session-user"),
        false,
        &[],
        &["turn-only"],
        ranking(Variant::Recent, Granularity::Session, 10, &[]),
    );
    assert!(score_longmemeval_retrieval(&[no_session_gold], Vec::new()).is_err());

    let bad_score = input(
        "q-nan",
        Some(1),
        None,
        false,
        &[],
        &["a"],
        ranking(Variant::Recent, Granularity::Turn, 10, &[("a", f64::NAN)]),
    );
    assert!(score_locomo_retrieval(&[bad_score], Vec::new()).is_err());

    let duplicate_hit = input(
        "q-duplicate",
        Some(1),
        None,
        false,
        &[],
        &["a"],
        ranking(
            Variant::Recent,
            Granularity::Turn,
            10,
            &[("a", 2.0), ("a", 1.0)],
        ),
    );
    assert!(score_locomo_retrieval(&[duplicate_hit], Vec::new()).is_err());
}

#[test]
fn exhaustive_small_rankings_prove_bounds_monotonicity_and_all_not_above_any() {
    let universe = ["a", "b", "c", "x"];
    for gold_mask in 1_u32..(1 << universe.len()) {
        let gold = universe
            .iter()
            .enumerate()
            .filter(|(index, _)| gold_mask & (1 << index) != 0)
            .map(|(_, id)| *id)
            .collect::<Vec<_>>();

        for hit_mask in 0_u32..(1 << universe.len()) {
            let hits = universe
                .iter()
                .enumerate()
                .filter(|(index, _)| hit_mask & (1 << index) != 0)
                .map(|(_, id)| *id)
                .collect::<Vec<_>>();
            let mut previous_recall = 0_u32;
            for k in 0..=universe.len() {
                let recall = memory_eval::metrics::recall_at_k(&gold, &hits, k);
                let all = recall_all_at_k(&gold, &hits, k);
                let any = recall_any_at_k(&gold, &hits, k);
                let ndcg = ndcg_at_k(&gold, &hits, k);

                assert!((0..=1000).contains(&recall));
                assert!(recall >= previous_recall);
                assert!((0.0..=1.0).contains(&all));
                assert!((0.0..=1.0).contains(&any));
                assert!((0.0..=1.0).contains(&ndcg));
                assert!(all <= any);
                assert!(all.is_finite() && any.is_finite() && ndcg.is_finite());
                previous_recall = recall;
            }
        }
    }
}

#[test]
fn every_aggregate_has_a_positive_denominator_and_finite_bounded_value() {
    let cases = vec![input(
        "q1",
        Some(1),
        None,
        true,
        &[],
        &["a", "b"],
        ranking(
            Variant::Oracle,
            Granularity::Turn,
            10,
            &[("a", 1.0), ("b", 1.0)],
        ),
    )];
    let report = score_locomo_retrieval(&cases, Vec::new()).unwrap();
    let metrics = report
        .overall
        .values()
        .chain(report.slices.values().flat_map(|slice| slice.values()));
    for metric in metrics {
        assert!(metric.denominator > 0);
        assert!(metric.numerator.is_finite());
        assert!(metric.value.is_finite());
        assert!((0.0..=metric.denominator as f64).contains(&metric.numerator));
        assert!((0.0..=1.0).contains(&metric.value));
    }

    let slice_names = report.slices.keys().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        slice_names,
        BTreeSet::from(["caption_evidence:true".to_owned(), "category:1".to_owned()])
    );
}
