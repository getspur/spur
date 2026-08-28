//! Pure, deterministic benchmark metrics over frozen ranking inputs.
//!
//! Ranking slices are consumed in their existing order. Metric computation
//! never sorts or otherwise tie-breaks retrieval evidence.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::Serialize;

const TOP_K: [usize; 3] = [1, 5, 10];

/// A typed failure produced while validating or aggregating metric inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricError {
    /// No eligible case exists in the requested aggregate.
    EmptyEligibleDenominator,
    /// An exact ratio or metric input has a zero denominator.
    ZeroDenominator,
    /// A bounded count has a numerator larger than its denominator.
    NumeratorExceedsDenominator {
        /// Supplied numerator.
        numerator: u128,
        /// Supplied denominator.
        denominator: u128,
    },
    /// A floating-point input or projection is not finite.
    NonFiniteValue {
        /// Stable input field name.
        field: &'static str,
    },
    /// A required grouping field is empty or whitespace-only.
    EmptyField {
        /// Stable input field name.
        field: &'static str,
    },
    /// An eligible case's key and suite-native input disagree.
    SuiteMismatch,
    /// An excluded case was incorrectly marked eligible.
    InvalidExcludedStatus,
    /// A nearest-rank percentile was requested without observations.
    EmptyOperationalSamples,
    /// Exact integer arithmetic exceeded the supported `u128` domain.
    ArithmeticOverflow,
}

impl fmt::Display for MetricError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEligibleDenominator => {
                formatter.write_str("metric aggregate has no eligible denominator")
            }
            Self::ZeroDenominator => formatter.write_str("metric denominator must be non-zero"),
            Self::NumeratorExceedsDenominator {
                numerator,
                denominator,
            } => write!(
                formatter,
                "metric numerator {numerator} exceeds denominator {denominator}"
            ),
            Self::NonFiniteValue { field } => {
                write!(formatter, "metric field {field} must be finite")
            }
            Self::EmptyField { field } => {
                write!(formatter, "metric field {field} must be non-empty")
            }
            Self::SuiteMismatch => {
                formatter.write_str("case key suite does not match suite-native input")
            }
            Self::InvalidExcludedStatus => {
                formatter.write_str("excluded metric case cannot have eligible status")
            }
            Self::EmptyOperationalSamples => {
                formatter.write_str("operational percentile requires at least one sample")
            }
            Self::ArithmeticOverflow => formatter.write_str("exact metric arithmetic overflowed"),
        }
    }
}

impl Error for MetricError {}

/// A reduced, exact non-negative rational number.
///
/// The integer representation is authoritative. [`Self::as_f64`] is a checked
/// convenience projection and never returns NaN or infinity.
///
/// # Examples
///
/// ```
/// use spur_code_eval::metrics::ExactRatio;
///
/// let ratio = ExactRatio::new(2, 6)?;
/// assert_eq!(ratio.numerator, 1);
/// assert_eq!(ratio.denominator, 3);
/// assert!(ratio.as_f64()?.is_finite());
/// # Ok::<(), spur_code_eval::metrics::MetricError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ExactRatio {
    /// Reduced numerator.
    pub numerator: u128,
    /// Reduced, strictly positive denominator.
    pub denominator: u128,
}

impl ExactRatio {
    /// Creates a reduced exact ratio.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::ZeroDenominator`] when `denominator` is zero.
    pub fn new(numerator: u128, denominator: u128) -> Result<Self, MetricError> {
        if denominator == 0 {
            return Err(MetricError::ZeroDenominator);
        }
        let divisor = greatest_common_divisor(numerator, denominator);
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    /// Returns a finite floating-point projection of the exact ratio.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::NonFiniteValue`] if the platform projection is
    /// unexpectedly non-finite.
    #[expect(
        clippy::cast_precision_loss,
        reason = "the exact integer fields remain authoritative; this API is an explicit projection"
    )]
    pub fn as_f64(self) -> Result<f64, MetricError> {
        let projection = self.numerator as f64 / self.denominator as f64;
        if projection.is_finite() {
            Ok(projection)
        } else {
            Err(MetricError::NonFiniteValue {
                field: "projection",
            })
        }
    }

    fn checked_add(self, other: Self) -> Result<Self, MetricError> {
        let shared = greatest_common_divisor(self.denominator, other.denominator);
        let self_multiplier = other.denominator / shared;
        let other_multiplier = self.denominator / shared;
        let left = self
            .numerator
            .checked_mul(self_multiplier)
            .ok_or(MetricError::ArithmeticOverflow)?;
        let right = other
            .numerator
            .checked_mul(other_multiplier)
            .ok_or(MetricError::ArithmeticOverflow)?;
        let numerator = left
            .checked_add(right)
            .ok_or(MetricError::ArithmeticOverflow)?;
        let denominator = self
            .denominator
            .checked_mul(self_multiplier)
            .ok_or(MetricError::ArithmeticOverflow)?;
        Self::new(numerator, denominator)
    }

    fn checked_divide_by(self, divisor: u128) -> Result<Self, MetricError> {
        if divisor == 0 {
            return Err(MetricError::ZeroDenominator);
        }
        let denominator = self
            .denominator
            .checked_mul(divisor)
            .ok_or(MetricError::ArithmeticOverflow)?;
        Self::new(self.numerator, denominator)
    }
}

/// Benchmark suite discriminator used by metric-only inputs and summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricSuite {
    /// `RepoQA` retrieval.
    RepoQa,
    /// `CrossCodeEval` cross-file evidence retrieval.
    CrossCodeEval,
    /// JCG call-graph expectations.
    Jcg,
}

/// Denominator-visible case status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    /// The case participates in suite-native metrics.
    Eligible,
    /// The case is valid, but its language or capability is unavailable.
    Unsupported,
    /// The case violates an input or gold invariant.
    Invalid,
}

/// Stable grouping identity for one case.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct CaseKey {
    /// Stable case identifier.
    pub case_id: String,
    /// Suite that owns the case's native metric.
    pub suite: MetricSuite,
    /// Suite-native slice or feature label.
    pub slice: String,
    /// Upstream language label.
    pub language: String,
    /// Immutable repository identity used by the benchmark.
    pub repository: String,
}

impl CaseKey {
    /// Creates a validated case grouping identity.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::EmptyField`] when a string field is empty.
    pub fn new(
        case_id: impl Into<String>,
        suite: MetricSuite,
        slice: impl Into<String>,
        language: impl Into<String>,
        repository: impl Into<String>,
    ) -> Result<Self, MetricError> {
        let case_id = case_id.into();
        let slice = slice.into();
        let language = language.into();
        let repository = repository.into();
        require_non_empty("case_id", &case_id)?;
        require_non_empty("slice", &slice)?;
        require_non_empty("language", &language)?;
        require_non_empty("repository", &repository)?;
        Ok(Self {
            case_id,
            suite,
            slice,
            language,
            repository,
        })
    }
}

/// One evidence item in its already-frozen rank position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RankedEvidence {
    score: f64,
    relevant: bool,
}

impl RankedEvidence {
    /// Creates one ranked evidence observation without changing its position.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::NonFiniteValue`] for NaN or infinite scores.
    pub fn new(score: f64, relevant: bool) -> Result<Self, MetricError> {
        if !score.is_finite() {
            return Err(MetricError::NonFiniteValue { field: "score" });
        }
        Ok(Self { score, relevant })
    }

    /// Returns the finite frozen score retained for auditing.
    #[must_use]
    pub const fn score(&self) -> f64 {
        self.score
    }

    /// Returns whether this frozen item matches gold evidence.
    #[must_use]
    pub const fn is_relevant(&self) -> bool {
        self.relevant
    }
}

/// Frozen retrieval ranking plus the exact number of gold evidence items.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetrievalInput {
    ranking: Vec<RankedEvidence>,
    gold_evidence: u128,
}

impl RetrievalInput {
    /// Creates a retrieval metric input.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::ZeroDenominator`] when `gold_evidence` is zero.
    pub fn new(ranking: Vec<RankedEvidence>, gold_evidence: u128) -> Result<Self, MetricError> {
        if gold_evidence == 0 {
            return Err(MetricError::ZeroDenominator);
        }
        Ok(Self {
            ranking,
            gold_evidence,
        })
    }

    /// Returns the frozen ranking in its original order.
    #[must_use]
    pub fn ranking(&self) -> &[RankedEvidence] {
        &self.ranking
    }

    /// Returns the exact gold evidence count.
    #[must_use]
    pub const fn gold_evidence(&self) -> u128 {
        self.gold_evidence
    }
}

/// `CrossCodeEval` retrieval and token-accounting input.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrossCodeEvalInput {
    retrieval: RetrievalInput,
    context_coverage: ExactRatio,
    token_budget_precision: ExactRatio,
}

impl CrossCodeEvalInput {
    /// Creates exact coverage and precision counters for one case.
    ///
    /// # Errors
    ///
    /// Returns a typed denominator or bounded-count error for invalid counts.
    pub fn new(
        retrieval: RetrievalInput,
        covered_context: u128,
        total_context: u128,
        relevant_tokens: u128,
        evidence_tokens: u128,
    ) -> Result<Self, MetricError> {
        Ok(Self {
            retrieval,
            context_coverage: bounded_ratio(covered_context, total_context)?,
            token_budget_precision: bounded_ratio(relevant_tokens, evidence_tokens)?,
        })
    }
}

/// JCG expectation counters for one case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JcgInput {
    expectations_passed: u128,
    expectations_total: u128,
    positive_targets: Option<(u128, u128)>,
    forbidden_target_violations: u128,
}

impl JcgInput {
    /// Creates exact JCG native counters.
    ///
    /// `positive_targets` is absent when annotation semantics do not permit the
    /// diagnostic. No global precision value is synthesized from partial gold.
    ///
    /// # Errors
    ///
    /// Returns a typed denominator or bounded-count error for invalid counts.
    pub fn new(
        expectations_passed: u128,
        expectations_total: u128,
        positive_targets: Option<(u128, u128)>,
        forbidden_target_violations: u128,
    ) -> Result<Self, MetricError> {
        bounded_ratio(expectations_passed, expectations_total)?;
        if let Some((found, total)) = positive_targets {
            bounded_ratio(found, total)?;
        }
        Ok(Self {
            expectations_passed,
            expectations_total,
            positive_targets,
            forbidden_target_violations,
        })
    }
}

/// Suite-native input, kept as a tagged union to prevent metric blending.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum SuiteCaseInput {
    /// `RepoQA` retrieval input.
    RepoQa(RetrievalInput),
    /// `CrossCodeEval` evidence input.
    CrossCodeEval(CrossCodeEvalInput),
    /// JCG expectation input.
    Jcg(JcgInput),
}

impl SuiteCaseInput {
    const fn suite(&self) -> MetricSuite {
        match self {
            Self::RepoQa(_) => MetricSuite::RepoQa,
            Self::CrossCodeEval(_) => MetricSuite::CrossCodeEval,
            Self::Jcg(_) => MetricSuite::Jcg,
        }
    }
}

/// One orthogonal operational state recorded for an eligible case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum OperationalSignal {
    /// Retrieval produced an answer-bearing result.
    Answered,
    /// Some requested evidence remained unresolved.
    Unresolved,
    /// Result normalization retained ambiguity.
    Ambiguous,
    /// A graph staleness signal was present.
    Stale,
}

impl OperationalSignal {
    const fn mask(self) -> u8 {
        match self {
            Self::Answered => 1 << 0,
            Self::Unresolved => 1 << 1,
            Self::Ambiguous => 1 << 2,
            Self::Stale => 1 << 3,
        }
    }
}

/// Compact set of orthogonal operational signals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct OperationalFlags(u8);

impl OperationalFlags {
    /// Creates a signal set from distinct signal labels.
    #[must_use]
    pub fn from_signals(signals: &[OperationalSignal]) -> Self {
        let bits = signals
            .iter()
            .fold(0_u8, |bits, signal| bits | signal.mask());
        Self(bits)
    }

    /// Returns whether this set contains `signal`.
    #[must_use]
    pub const fn contains(self, signal: OperationalSignal) -> bool {
        self.0 & signal.mask() != 0
    }
}

/// Common operational observations for one eligible case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperationalInput {
    /// End-to-end query latency in microseconds.
    pub latency_micros: u64,
    /// Evidence bytes included for the case.
    pub evidence_bytes: u64,
    /// Evidence tokens included for the case.
    pub evidence_tokens: u64,
    /// Compact operational status signals.
    pub flags: OperationalFlags,
}

impl OperationalInput {
    /// Creates one operational observation.
    #[must_use]
    pub const fn new(
        latency_micros: u64,
        evidence_bytes: u64,
        evidence_tokens: u64,
        flags: OperationalFlags,
    ) -> Self {
        Self {
            latency_micros,
            evidence_bytes,
            evidence_tokens,
            flags,
        }
    }
}

/// Validated per-case input for publication aggregation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseMetricInput {
    key: CaseKey,
    status: CaseStatus,
    suite_input: Option<SuiteCaseInput>,
    operational: Option<OperationalInput>,
}

impl CaseMetricInput {
    /// Creates an eligible case with suite-native and operational inputs.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::SuiteMismatch`] when the key and input suites
    /// differ.
    pub fn eligible(
        key: CaseKey,
        suite_input: SuiteCaseInput,
        operational: OperationalInput,
    ) -> Result<Self, MetricError> {
        if key.suite != suite_input.suite() {
            return Err(MetricError::SuiteMismatch);
        }
        Ok(Self {
            key,
            status: CaseStatus::Eligible,
            suite_input: Some(suite_input),
            operational: Some(operational),
        })
    }

    /// Creates a denominator-visible unsupported or invalid case.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::InvalidExcludedStatus`] for an eligible status.
    pub fn excluded(key: CaseKey, status: CaseStatus) -> Result<Self, MetricError> {
        if status == CaseStatus::Eligible {
            return Err(MetricError::InvalidExcludedStatus);
        }
        Ok(Self {
            key,
            status,
            suite_input: None,
            operational: None,
        })
    }
}

/// Exact retrieval metrics averaged across their visible case denominator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetrievalMetrics {
    /// Hit@1.
    pub hit_at_1: ExactRatio,
    /// Hit@5.
    pub hit_at_5: ExactRatio,
    /// Hit@10.
    pub hit_at_10: ExactRatio,
    /// Recall@1.
    pub recall_at_1: ExactRatio,
    /// Recall@5.
    pub recall_at_5: ExactRatio,
    /// Recall@10.
    pub recall_at_10: ExactRatio,
    /// Mean reciprocal rank.
    pub mrr: ExactRatio,
}

/// `CrossCodeEval`-native metrics, separate from other suite projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrossCodeEvalMetrics {
    /// Common retrieval quality.
    pub retrieval: RetrievalMetrics,
    /// Mean exact context coverage.
    pub context_coverage: ExactRatio,
    /// Mean exact evidence precision at the frozen token budget.
    pub token_budget_precision: ExactRatio,
}

/// JCG-native expectation and annotation-safe diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JcgMetrics {
    /// Passed expectations before ratio reduction.
    pub expectations_passed: u128,
    /// Total expectations before ratio reduction.
    pub expectations_total: u128,
    /// Exact upstream expectation pass rate.
    pub expectation_pass_rate: ExactRatio,
    /// Positive targets found when annotation semantics permit recall.
    pub positive_targets_found: Option<u128>,
    /// Total positive targets when annotation semantics permit recall.
    pub positive_targets_total: Option<u128>,
    /// Exact positive-target recall, absent for partial annotations.
    pub positive_target_recall: Option<ExactRatio>,
    /// Number of prohibited target matches.
    pub forbidden_target_violations: u128,
}

/// Suite-native output, structurally preventing unlike metric blending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SuiteMetrics {
    /// `RepoQA` retrieval metrics.
    RepoQa(RetrievalMetrics),
    /// `CrossCodeEval` evidence metrics.
    CrossCodeEval(CrossCodeEvalMetrics),
    /// JCG expectation metrics.
    Jcg(JcgMetrics),
}

/// Deterministic nearest-rank p50 and p95 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Percentiles {
    /// Nearest-rank 50th percentile.
    pub p50: u64,
    /// Nearest-rank 95th percentile.
    pub p95: u64,
}

/// All visible case and operational denominators for one aggregate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Denominators {
    /// Every upstream case in the aggregate.
    pub total: u64,
    /// Cases that participate in native scoring.
    pub eligible: u64,
    /// Valid but unsupported cases.
    pub unsupported: u64,
    /// Invalid cases.
    pub invalid: u64,
    /// Eligible cases with an answer-bearing result.
    pub answered: u64,
    /// Eligible cases with unresolved evidence.
    pub unresolved: u64,
    /// Eligible cases with ambiguity.
    pub ambiguous: u64,
    /// Eligible cases with graph staleness.
    pub stale: u64,
}

/// Common operational summary over eligible cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationalMetrics {
    /// Answered eligible cases divided by eligible cases.
    pub answer_rate: ExactRatio,
    /// Unsupported cases divided by all visible cases.
    pub unsupported_rate: ExactRatio,
    /// Invalid cases divided by all visible cases.
    pub invalid_rate: ExactRatio,
    /// Unresolved eligible cases divided by eligible cases.
    pub unresolved_rate: ExactRatio,
    /// Ambiguous eligible cases divided by eligible cases.
    pub ambiguity_rate: ExactRatio,
    /// Stale eligible cases divided by eligible cases.
    pub staleness_rate: ExactRatio,
    /// Query latency percentiles in microseconds.
    pub latency_micros: Percentiles,
    /// Evidence byte percentiles.
    pub evidence_bytes: Percentiles,
    /// Evidence token percentiles.
    pub evidence_tokens: Percentiles,
}

/// One suite's denominator-visible, non-blended contribution to an aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuiteAggregate {
    /// Suite identity.
    pub suite: MetricSuite,
    /// Denominators scoped to this suite.
    pub denominators: Denominators,
    /// Suite-native metrics, absent when this suite has no eligible case.
    pub metrics: Option<SuiteMetrics>,
}

/// Shared aggregate shape used by grouping levels and the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AggregateSummary {
    /// Complete denominator counters.
    pub denominators: Denominators,
    /// Operational metrics, absent for an excluded-only subgroup.
    pub operational: Option<OperationalMetrics>,
    /// Per-suite native metrics in deterministic suite order.
    pub suites: Vec<SuiteAggregate>,
}

/// Per-case publication row in original case order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseSummary {
    /// Case grouping identity.
    pub key: CaseKey,
    /// Denominator-visible status.
    pub status: CaseStatus,
    /// Suite-native metrics for eligible cases.
    pub metrics: Option<SuiteMetrics>,
    /// Operational input retained for eligible cases.
    pub operational: Option<OperationalInput>,
}

/// Key for the combined language/repository aggregation level.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct LanguageRepositoryKey {
    /// Upstream language label.
    pub language: String,
    /// Immutable repository identity.
    pub repository: String,
}

/// Key for the combined suite/slice aggregation level.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct SuiteSliceKey {
    /// Suite identity.
    pub suite: MetricSuite,
    /// Suite-native slice or feature label.
    pub slice: String,
}

/// One deterministic grouped summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupedSummary<K> {
    /// Group identity.
    pub key: K,
    /// Metrics and denominators for the group.
    pub summary: AggregateSummary,
}

/// Complete four-level publication payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishedMetrics {
    /// Per-case rows, preserving caller order.
    pub per_case: Vec<CaseSummary>,
    /// Language/repository groups in canonical key order.
    pub language_repository: Vec<GroupedSummary<LanguageRepositoryKey>>,
    /// Suite/slice groups in canonical key order.
    pub suite_slice: Vec<GroupedSummary<SuiteSliceKey>>,
    /// Non-blended dashboard summary.
    pub dashboard: AggregateSummary,
}

/// Computes exact Hit@1/5/10, Recall@1/5/10, and reciprocal rank.
///
/// Ties retain the supplied frozen order because this function never compares
/// scores or sorts `input.ranking()`.
///
/// # Errors
///
/// Returns a typed bounded-count or arithmetic error if a corrupted ranking
/// contains more relevant items than the declared gold denominator.
pub fn score_retrieval(input: &RetrievalInput) -> Result<RetrievalMetrics, MetricError> {
    let hit_at_1 = hit_at(input, TOP_K[0])?;
    let hit_at_5 = hit_at(input, TOP_K[1])?;
    let hit_at_10 = hit_at(input, TOP_K[2])?;
    let recall_at_1 = recall_at(input, TOP_K[0])?;
    let recall_at_5 = recall_at(input, TOP_K[1])?;
    let recall_at_10 = recall_at(input, TOP_K[2])?;
    let mrr = match input.ranking.iter().position(RankedEvidence::is_relevant) {
        Some(index) => {
            let rank = index
                .checked_add(1)
                .and_then(|value| u128::try_from(value).ok())
                .ok_or(MetricError::ArithmeticOverflow)?;
            ExactRatio::new(1, rank)?
        }
        None => ExactRatio::new(0, 1)?,
    };
    Ok(RetrievalMetrics {
        hit_at_1,
        hit_at_5,
        hit_at_10,
        recall_at_1,
        recall_at_5,
        recall_at_10,
        mrr,
    })
}

/// Computes deterministic nearest-rank p50 and p95 over integer observations.
///
/// The function sorts an internal copy of operational samples; it never sees
/// or mutates frozen retrieval rankings.
///
/// # Errors
///
/// Returns [`MetricError::EmptyOperationalSamples`] for an empty slice and a
/// typed overflow if percentile-index arithmetic cannot be represented.
pub fn nearest_rank_percentiles(samples: &[u64]) -> Result<Percentiles, MetricError> {
    if samples.is_empty() {
        return Err(MetricError::EmptyOperationalSamples);
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    Ok(Percentiles {
        p50: ordered[nearest_rank_index(ordered.len(), 50)?],
        p95: ordered[nearest_rank_index(ordered.len(), 95)?],
    })
}

/// Publishes per-case, language/repository, suite/slice, and dashboard metrics.
///
/// # Errors
///
/// Returns [`MetricError::EmptyEligibleDenominator`] instead of producing an
/// empty or NaN aggregate. Other typed errors report corrupted exact counters.
pub fn aggregate_metrics(cases: &[CaseMetricInput]) -> Result<PublishedMetrics, MetricError> {
    let dashboard_denominators = denominators(cases.iter().collect::<Vec<_>>().as_slice())?;
    if dashboard_denominators.eligible == 0 {
        return Err(MetricError::EmptyEligibleDenominator);
    }

    let per_case = cases
        .iter()
        .map(case_summary)
        .collect::<Result<Vec<_>, _>>()?;

    let mut language_groups: BTreeMap<LanguageRepositoryKey, Vec<&CaseMetricInput>> =
        BTreeMap::new();
    let mut suite_groups: BTreeMap<SuiteSliceKey, Vec<&CaseMetricInput>> = BTreeMap::new();
    for case in cases {
        language_groups
            .entry(LanguageRepositoryKey {
                language: case.key.language.clone(),
                repository: case.key.repository.clone(),
            })
            .or_default()
            .push(case);
        suite_groups
            .entry(SuiteSliceKey {
                suite: case.key.suite,
                slice: case.key.slice.clone(),
            })
            .or_default()
            .push(case);
    }

    let language_repository = build_grouped(language_groups)?;
    let suite_slice = build_grouped(suite_groups)?;
    let all_cases: Vec<_> = cases.iter().collect();
    let dashboard = aggregate_refs(&all_cases)?;
    Ok(PublishedMetrics {
        per_case,
        language_repository,
        suite_slice,
        dashboard,
    })
}

fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn require_non_empty(field: &'static str, value: &str) -> Result<(), MetricError> {
    if value.trim().is_empty() {
        Err(MetricError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn bounded_ratio(numerator: u128, denominator: u128) -> Result<ExactRatio, MetricError> {
    if numerator > denominator {
        return Err(MetricError::NumeratorExceedsDenominator {
            numerator,
            denominator,
        });
    }
    ExactRatio::new(numerator, denominator)
}

fn hit_at(input: &RetrievalInput, limit: usize) -> Result<ExactRatio, MetricError> {
    let hit = input
        .ranking
        .iter()
        .take(limit)
        .any(RankedEvidence::is_relevant);
    ExactRatio::new(u128::from(hit), 1)
}

fn recall_at(input: &RetrievalInput, limit: usize) -> Result<ExactRatio, MetricError> {
    let relevant = input
        .ranking
        .iter()
        .take(limit)
        .filter(|evidence| evidence.is_relevant())
        .count();
    let relevant =
        u128::try_from(relevant).map_err(|_conversion_error| MetricError::ArithmeticOverflow)?;
    bounded_ratio(relevant, input.gold_evidence)
}

fn nearest_rank_index(length: usize, percentile: usize) -> Result<usize, MetricError> {
    let scaled = length
        .checked_mul(percentile)
        .ok_or(MetricError::ArithmeticOverflow)?;
    scaled
        .div_ceil(100)
        .checked_sub(1)
        .ok_or(MetricError::ArithmeticOverflow)
}

fn case_summary(case: &CaseMetricInput) -> Result<CaseSummary, MetricError> {
    let metrics = case
        .suite_input
        .as_ref()
        .map(score_suite_case)
        .transpose()?;
    Ok(CaseSummary {
        key: case.key.clone(),
        status: case.status,
        metrics,
        operational: case.operational,
    })
}

fn score_suite_case(input: &SuiteCaseInput) -> Result<SuiteMetrics, MetricError> {
    match input {
        SuiteCaseInput::RepoQa(retrieval) => Ok(SuiteMetrics::RepoQa(score_retrieval(retrieval)?)),
        SuiteCaseInput::CrossCodeEval(cross) => {
            Ok(SuiteMetrics::CrossCodeEval(CrossCodeEvalMetrics {
                retrieval: score_retrieval(&cross.retrieval)?,
                context_coverage: cross.context_coverage,
                token_budget_precision: cross.token_budget_precision,
            }))
        }
        SuiteCaseInput::Jcg(jcg) => Ok(SuiteMetrics::Jcg(score_jcg(jcg)?)),
    }
}

fn score_jcg(input: &JcgInput) -> Result<JcgMetrics, MetricError> {
    let (positive_targets_found, positive_targets_total, positive_target_recall) =
        match input.positive_targets {
            Some((found, total)) => (Some(found), Some(total), Some(bounded_ratio(found, total)?)),
            None => (None, None, None),
        };
    Ok(JcgMetrics {
        expectations_passed: input.expectations_passed,
        expectations_total: input.expectations_total,
        expectation_pass_rate: bounded_ratio(input.expectations_passed, input.expectations_total)?,
        positive_targets_found,
        positive_targets_total,
        positive_target_recall,
        forbidden_target_violations: input.forbidden_target_violations,
    })
}

fn build_grouped<K: Ord>(
    groups: BTreeMap<K, Vec<&CaseMetricInput>>,
) -> Result<Vec<GroupedSummary<K>>, MetricError> {
    groups
        .into_iter()
        .map(|(key, cases)| {
            Ok(GroupedSummary {
                key,
                summary: aggregate_refs(&cases)?,
            })
        })
        .collect()
}

fn aggregate_refs(cases: &[&CaseMetricInput]) -> Result<AggregateSummary, MetricError> {
    let summary_denominators = denominators(cases)?;
    let operational = if summary_denominators.eligible == 0 {
        None
    } else {
        Some(operational_metrics(cases, summary_denominators)?)
    };

    let mut suites: BTreeMap<MetricSuite, Vec<&CaseMetricInput>> = BTreeMap::new();
    for case in cases {
        suites.entry(case.key.suite).or_default().push(case);
    }
    let suites = suites
        .into_iter()
        .map(|(suite, suite_cases)| {
            let suite_denominators = denominators(&suite_cases)?;
            let metrics = if suite_denominators.eligible == 0 {
                None
            } else {
                Some(aggregate_suite(suite, &suite_cases)?)
            };
            Ok(SuiteAggregate {
                suite,
                denominators: suite_denominators,
                metrics,
            })
        })
        .collect::<Result<Vec<_>, MetricError>>()?;

    Ok(AggregateSummary {
        denominators: summary_denominators,
        operational,
        suites,
    })
}

fn denominators(cases: &[&CaseMetricInput]) -> Result<Denominators, MetricError> {
    let mut result = Denominators::default();
    for case in cases {
        increment(&mut result.total)?;
        match case.status {
            CaseStatus::Eligible => {
                increment(&mut result.eligible)?;
                let operational = case.operational.ok_or(MetricError::SuiteMismatch)?;
                if operational.flags.contains(OperationalSignal::Answered) {
                    increment(&mut result.answered)?;
                }
                if operational.flags.contains(OperationalSignal::Unresolved) {
                    increment(&mut result.unresolved)?;
                }
                if operational.flags.contains(OperationalSignal::Ambiguous) {
                    increment(&mut result.ambiguous)?;
                }
                if operational.flags.contains(OperationalSignal::Stale) {
                    increment(&mut result.stale)?;
                }
            }
            CaseStatus::Unsupported => increment(&mut result.unsupported)?,
            CaseStatus::Invalid => increment(&mut result.invalid)?,
        }
    }
    Ok(result)
}

fn increment(value: &mut u64) -> Result<(), MetricError> {
    *value = value
        .checked_add(1)
        .ok_or(MetricError::ArithmeticOverflow)?;
    Ok(())
}

fn operational_metrics(
    cases: &[&CaseMetricInput],
    denominators: Denominators,
) -> Result<OperationalMetrics, MetricError> {
    let eligible: Vec<_> = cases.iter().filter_map(|case| case.operational).collect();
    if eligible.is_empty() {
        return Err(MetricError::EmptyEligibleDenominator);
    }
    let latency: Vec<_> = eligible.iter().map(|input| input.latency_micros).collect();
    let bytes: Vec<_> = eligible.iter().map(|input| input.evidence_bytes).collect();
    let tokens: Vec<_> = eligible.iter().map(|input| input.evidence_tokens).collect();
    Ok(OperationalMetrics {
        answer_rate: count_ratio(denominators.answered, denominators.eligible)?,
        unsupported_rate: count_ratio(denominators.unsupported, denominators.total)?,
        invalid_rate: count_ratio(denominators.invalid, denominators.total)?,
        unresolved_rate: count_ratio(denominators.unresolved, denominators.eligible)?,
        ambiguity_rate: count_ratio(denominators.ambiguous, denominators.eligible)?,
        staleness_rate: count_ratio(denominators.stale, denominators.eligible)?,
        latency_micros: nearest_rank_percentiles(&latency)?,
        evidence_bytes: nearest_rank_percentiles(&bytes)?,
        evidence_tokens: nearest_rank_percentiles(&tokens)?,
    })
}

fn count_ratio(numerator: u64, denominator: u64) -> Result<ExactRatio, MetricError> {
    bounded_ratio(u128::from(numerator), u128::from(denominator))
}

fn aggregate_suite(
    suite: MetricSuite,
    cases: &[&CaseMetricInput],
) -> Result<SuiteMetrics, MetricError> {
    match suite {
        MetricSuite::RepoQa => {
            let retrievals = eligible_suite_inputs(cases).map(|input| match input {
                SuiteCaseInput::RepoQa(retrieval) => Ok(retrieval),
                _ => Err(MetricError::SuiteMismatch),
            });
            Ok(SuiteMetrics::RepoQa(aggregate_retrieval(retrievals)?))
        }
        MetricSuite::CrossCodeEval => {
            let cross = eligible_suite_inputs(cases)
                .map(|input| match input {
                    SuiteCaseInput::CrossCodeEval(cross) => Ok(cross),
                    _ => Err(MetricError::SuiteMismatch),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let retrievals = cross.iter().copied().map(|input| Ok(&input.retrieval));
            Ok(SuiteMetrics::CrossCodeEval(CrossCodeEvalMetrics {
                retrieval: aggregate_retrieval(retrievals)?,
                context_coverage: mean_exact(cross.iter().map(|input| input.context_coverage))?,
                token_budget_precision: mean_exact(
                    cross.iter().map(|input| input.token_budget_precision),
                )?,
            }))
        }
        MetricSuite::Jcg => {
            let inputs = eligible_suite_inputs(cases)
                .map(|input| match input {
                    SuiteCaseInput::Jcg(jcg) => Ok(jcg),
                    _ => Err(MetricError::SuiteMismatch),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(SuiteMetrics::Jcg(aggregate_jcg(&inputs)?))
        }
    }
}

fn eligible_suite_inputs<'a>(
    cases: &'a [&'a CaseMetricInput],
) -> impl Iterator<Item = &'a SuiteCaseInput> {
    cases.iter().filter_map(|case| case.suite_input.as_ref())
}

fn aggregate_retrieval<'a>(
    inputs: impl Iterator<Item = Result<&'a RetrievalInput, MetricError>>,
) -> Result<RetrievalMetrics, MetricError> {
    let scored = inputs
        .map(|input| score_retrieval(input?))
        .collect::<Result<Vec<_>, MetricError>>()?;
    Ok(RetrievalMetrics {
        hit_at_1: mean_exact(scored.iter().map(|metrics| metrics.hit_at_1))?,
        hit_at_5: mean_exact(scored.iter().map(|metrics| metrics.hit_at_5))?,
        hit_at_10: mean_exact(scored.iter().map(|metrics| metrics.hit_at_10))?,
        recall_at_1: mean_exact(scored.iter().map(|metrics| metrics.recall_at_1))?,
        recall_at_5: mean_exact(scored.iter().map(|metrics| metrics.recall_at_5))?,
        recall_at_10: mean_exact(scored.iter().map(|metrics| metrics.recall_at_10))?,
        mrr: mean_exact(scored.iter().map(|metrics| metrics.mrr))?,
    })
}

fn aggregate_jcg(inputs: &[&JcgInput]) -> Result<JcgMetrics, MetricError> {
    let mut expectations_passed = 0_u128;
    let mut expectations_total = 0_u128;
    let mut positive_targets_found = 0_u128;
    let mut positive_targets_total = 0_u128;
    let mut has_positive_targets = false;
    let mut forbidden_target_violations = 0_u128;
    for input in inputs {
        expectations_passed = checked_sum(expectations_passed, input.expectations_passed)?;
        expectations_total = checked_sum(expectations_total, input.expectations_total)?;
        if let Some((found, total)) = input.positive_targets {
            has_positive_targets = true;
            positive_targets_found = checked_sum(positive_targets_found, found)?;
            positive_targets_total = checked_sum(positive_targets_total, total)?;
        }
        forbidden_target_violations = checked_sum(
            forbidden_target_violations,
            input.forbidden_target_violations,
        )?;
    }
    let (positive_found, positive_total, positive_recall) = if has_positive_targets {
        (
            Some(positive_targets_found),
            Some(positive_targets_total),
            Some(bounded_ratio(
                positive_targets_found,
                positive_targets_total,
            )?),
        )
    } else {
        (None, None, None)
    };
    Ok(JcgMetrics {
        expectations_passed,
        expectations_total,
        expectation_pass_rate: bounded_ratio(expectations_passed, expectations_total)?,
        positive_targets_found: positive_found,
        positive_targets_total: positive_total,
        positive_target_recall: positive_recall,
        forbidden_target_violations,
    })
}

fn checked_sum(left: u128, right: u128) -> Result<u128, MetricError> {
    left.checked_add(right)
        .ok_or(MetricError::ArithmeticOverflow)
}

fn mean_exact(values: impl Iterator<Item = ExactRatio>) -> Result<ExactRatio, MetricError> {
    let mut sum = ExactRatio::new(0, 1)?;
    let mut count = 0_u128;
    for value in values {
        sum = sum.checked_add(value)?;
        count = count
            .checked_add(1)
            .ok_or(MetricError::ArithmeticOverflow)?;
    }
    if count == 0 {
        return Err(MetricError::EmptyEligibleDenominator);
    }
    sum.checked_divide_by(count)
}
