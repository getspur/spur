#[path = "../src/metrics.rs"]
mod metrics;

use metrics::{
    aggregate_metrics, nearest_rank_percentiles, score_retrieval, CaseKey, CaseMetricInput,
    CaseStatus, CrossCodeEvalInput, ExactRatio, JcgInput, MetricError, MetricSuite,
    OperationalFlags, OperationalInput, OperationalSignal, RankedEvidence, RetrievalInput,
    SuiteCaseInput, SuiteMetrics,
};

fn ratio(numerator: u128, denominator: u128) -> ExactRatio {
    ExactRatio::new(numerator, denominator).unwrap()
}

fn key(
    case_id: &str,
    suite: MetricSuite,
    slice: &str,
    language: &str,
    repository: &str,
) -> CaseKey {
    CaseKey::new(case_id, suite, slice, language, repository).unwrap()
}

fn ranking(relevant_rank: Option<usize>) -> Vec<RankedEvidence> {
    (1..=10)
        .map(|rank| {
            let score = 11.0 - f64::from(u32::try_from(rank).unwrap());
            RankedEvidence::new(score, relevant_rank == Some(rank)).unwrap()
        })
        .collect()
}

fn operational(
    latency_micros: u64,
    evidence_bytes: u64,
    evidence_tokens: u64,
    signals: &[OperationalSignal],
) -> OperationalInput {
    OperationalInput::new(
        latency_micros,
        evidence_bytes,
        evidence_tokens,
        OperationalFlags::from_signals(signals),
    )
}

fn repo_qa_case(
    case_id: &str,
    language: &str,
    repository: &str,
    relevant_rank: Option<usize>,
    operational: OperationalInput,
) -> CaseMetricInput {
    CaseMetricInput::eligible(
        key(
            case_id,
            MetricSuite::RepoQa,
            "retrieval",
            language,
            repository,
        ),
        SuiteCaseInput::RepoQa(RetrievalInput::new(ranking(relevant_rank), 1).unwrap()),
        operational,
    )
    .unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one hand-calculated fixture verifies its complete publication projection"
)]
fn hand_calculated_metrics_and_denominators_are_exact() {
    let cases = vec![
        repo_qa_case(
            "rank-1",
            "python",
            "repo-a",
            Some(1),
            operational(10, 100, 10, &[OperationalSignal::Answered]),
        ),
        repo_qa_case(
            "rank-3",
            "python",
            "repo-a",
            Some(3),
            operational(
                20,
                200,
                20,
                &[
                    OperationalSignal::Answered,
                    OperationalSignal::Unresolved,
                    OperationalSignal::Stale,
                ],
            ),
        ),
        repo_qa_case(
            "missing",
            "rust",
            "repo-b",
            None,
            operational(30, 300, 30, &[OperationalSignal::Ambiguous]),
        ),
        CaseMetricInput::excluded(
            key(
                "unsupported",
                MetricSuite::CrossCodeEval,
                "cross-file",
                "java",
                "repo-c",
            ),
            CaseStatus::Unsupported,
        )
        .unwrap(),
        CaseMetricInput::excluded(
            key(
                "invalid",
                MetricSuite::Jcg,
                "direct",
                "javascript",
                "repo-d",
            ),
            CaseStatus::Invalid,
        )
        .unwrap(),
    ];

    let published = aggregate_metrics(&cases).unwrap();
    let dashboard = &published.dashboard;

    assert_eq!(dashboard.denominators.total, 5);
    assert_eq!(dashboard.denominators.eligible, 3);
    assert_eq!(dashboard.denominators.unsupported, 1);
    assert_eq!(dashboard.denominators.invalid, 1);
    assert_eq!(dashboard.denominators.answered, 2);
    assert_eq!(dashboard.denominators.unresolved, 1);
    assert_eq!(dashboard.denominators.ambiguous, 1);
    assert_eq!(dashboard.denominators.stale, 1);

    let operational = dashboard.operational.as_ref().unwrap();
    assert_eq!(operational.answer_rate, ratio(2, 3));
    assert_eq!(operational.unsupported_rate, ratio(1, 5));
    assert_eq!(operational.invalid_rate, ratio(1, 5));
    assert_eq!(operational.unresolved_rate, ratio(1, 3));
    assert_eq!(operational.ambiguity_rate, ratio(1, 3));
    assert_eq!(operational.staleness_rate, ratio(1, 3));
    assert_eq!(operational.latency_micros.p50, 20);
    assert_eq!(operational.latency_micros.p95, 30);
    assert_eq!(operational.evidence_bytes.p50, 200);
    assert_eq!(operational.evidence_bytes.p95, 300);
    assert_eq!(operational.evidence_tokens.p50, 20);
    assert_eq!(operational.evidence_tokens.p95, 30);

    let repo_qa = dashboard
        .suites
        .iter()
        .find(|summary| summary.suite == MetricSuite::RepoQa)
        .unwrap();
    let SuiteMetrics::RepoQa(metrics) = repo_qa.metrics.as_ref().unwrap() else {
        panic!("RepoQA summary must retain RepoQA-native metrics");
    };
    assert_eq!(metrics.hit_at_1, ratio(1, 3));
    assert_eq!(metrics.hit_at_5, ratio(2, 3));
    assert_eq!(metrics.hit_at_10, ratio(2, 3));
    assert_eq!(metrics.recall_at_1, ratio(1, 3));
    assert_eq!(metrics.recall_at_5, ratio(2, 3));
    assert_eq!(metrics.recall_at_10, ratio(2, 3));
    assert_eq!(metrics.mrr, ratio(4, 9));

    let per_case_order: Vec<_> = published
        .per_case
        .iter()
        .map(|summary| summary.key.case_id.as_str())
        .collect();
    assert_eq!(
        per_case_order,
        ["rank-1", "rank-3", "missing", "unsupported", "invalid"]
    );
    assert_eq!(published.language_repository.len(), 4);
    assert_eq!(published.suite_slice.len(), 3);
    assert_eq!(dashboard.suites.len(), 3);
    assert!(dashboard
        .suites
        .iter()
        .find(|summary| summary.suite == MetricSuite::CrossCodeEval)
        .unwrap()
        .metrics
        .is_none());
}

#[test]
fn crosscodeeval_recall_coverage_and_token_precision_are_exact() {
    let retrieval = RetrievalInput::new(
        vec![
            RankedEvidence::new(1.0, false).unwrap(),
            RankedEvidence::new(0.9, true).unwrap(),
            RankedEvidence::new(0.8, false).unwrap(),
            RankedEvidence::new(0.7, false).unwrap(),
            RankedEvidence::new(0.6, true).unwrap(),
        ],
        4,
    )
    .unwrap();
    let quality = CrossCodeEvalInput::new(retrieval, 3, 4, 7, 10).unwrap();
    let case = CaseMetricInput::eligible(
        key(
            "cross",
            MetricSuite::CrossCodeEval,
            "cross-file",
            "python",
            "repo-a",
        ),
        SuiteCaseInput::CrossCodeEval(quality),
        operational(
            17,
            120,
            10,
            &[
                OperationalSignal::Answered,
                OperationalSignal::Unresolved,
                OperationalSignal::Ambiguous,
                OperationalSignal::Stale,
            ],
        ),
    )
    .unwrap();

    let published = aggregate_metrics(&[case]).unwrap();
    let cross = published
        .dashboard
        .suites
        .iter()
        .find(|summary| summary.suite == MetricSuite::CrossCodeEval)
        .unwrap();
    let SuiteMetrics::CrossCodeEval(metrics) = cross.metrics.as_ref().unwrap() else {
        panic!("CrossCodeEval summary must retain CrossCodeEval-native metrics");
    };

    assert_eq!(metrics.retrieval.hit_at_1, ratio(0, 1));
    assert_eq!(metrics.retrieval.hit_at_5, ratio(1, 1));
    assert_eq!(metrics.retrieval.recall_at_1, ratio(0, 1));
    assert_eq!(metrics.retrieval.recall_at_5, ratio(1, 2));
    assert_eq!(metrics.retrieval.mrr, ratio(1, 2));
    assert_eq!(metrics.context_coverage, ratio(3, 4));
    assert_eq!(metrics.token_budget_precision, ratio(7, 10));
}

#[test]
fn ties_keep_frozen_order_and_nearest_rank_is_deterministic() {
    let retrieval = RetrievalInput::new(
        vec![
            RankedEvidence::new(0.75, false).unwrap(),
            RankedEvidence::new(0.75, true).unwrap(),
            RankedEvidence::new(0.75, false).unwrap(),
        ],
        1,
    )
    .unwrap();

    assert_eq!(retrieval.ranking()[0].score().to_bits(), 0.75_f64.to_bits());
    assert_eq!(retrieval.gold_evidence(), 1);
    let metrics = score_retrieval(&retrieval).unwrap();
    assert_eq!(metrics.hit_at_1, ratio(0, 1));
    assert_eq!(metrics.mrr, ratio(1, 2));

    let percentiles = nearest_rank_percentiles(&[40, 10, 30, 20]).unwrap();
    assert_eq!(percentiles.p50, 20);
    assert_eq!(percentiles.p95, 40);
}

#[test]
fn suite_native_metrics_remain_non_blended() {
    let repo_qa = repo_qa_case(
        "repo",
        "python",
        "repo-a",
        Some(1),
        operational(1, 10, 1, &[OperationalSignal::Answered]),
    );
    let cross = CaseMetricInput::eligible(
        key(
            "cross",
            MetricSuite::CrossCodeEval,
            "cross-file",
            "python",
            "repo-a",
        ),
        SuiteCaseInput::CrossCodeEval(
            CrossCodeEvalInput::new(
                RetrievalInput::new(ranking(Some(2)), 1).unwrap(),
                1,
                2,
                3,
                5,
            )
            .unwrap(),
        ),
        operational(2, 20, 2, &[OperationalSignal::Answered]),
    )
    .unwrap();
    let jcg = CaseMetricInput::eligible(
        key("jcg", MetricSuite::Jcg, "direct", "javascript", "repo-b"),
        SuiteCaseInput::Jcg(JcgInput::new(3, 4, Some((2, 3)), 1).unwrap()),
        operational(3, 30, 3, &[OperationalSignal::Answered]),
    )
    .unwrap();

    let published = aggregate_metrics(&[repo_qa, cross, jcg]).unwrap();
    assert_eq!(published.dashboard.suites.len(), 3);
    assert!(matches!(
        published.dashboard.suites[0].metrics,
        Some(SuiteMetrics::RepoQa(_))
    ));
    assert!(matches!(
        published.dashboard.suites[1].metrics,
        Some(SuiteMetrics::CrossCodeEval(_))
    ));
    let Some(SuiteMetrics::Jcg(metrics)) = &published.dashboard.suites[2].metrics else {
        panic!("JCG summary must retain JCG-native metrics");
    };
    assert_eq!(metrics.expectation_pass_rate, ratio(3, 4));
    assert_eq!(metrics.positive_target_recall, Some(ratio(2, 3)));
    assert_eq!(metrics.forbidden_target_violations, 1);
}

#[test]
fn empty_denominators_and_non_finite_values_are_typed_errors() {
    let unsupported = CaseMetricInput::excluded(
        key(
            "unsupported",
            MetricSuite::RepoQa,
            "retrieval",
            "java",
            "repo-a",
        ),
        CaseStatus::Unsupported,
    )
    .unwrap();

    assert_eq!(
        aggregate_metrics(&[unsupported]),
        Err(MetricError::EmptyEligibleDenominator)
    );
    assert_eq!(ExactRatio::new(1, 0), Err(MetricError::ZeroDenominator));
    assert_eq!(
        RankedEvidence::new(f64::NAN, true),
        Err(MetricError::NonFiniteValue { field: "score" })
    );

    let projection = ExactRatio::new(u128::MAX, u128::MAX)
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(projection.is_finite());
}
