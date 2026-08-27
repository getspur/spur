use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use serde::{ser::SerializeStruct, Deserialize, Serialize, Serializer};

use super::{
    contract::DatasetKind,
    ranking::{Granularity, Ranking, Variant},
};

#[allow(dead_code)]
const LOCOMO_CUTOFFS: [usize; 3] = [1, 5, 10];
#[allow(dead_code)]
const LONGMEM_SESSION_CUTOFFS: [usize; 2] = [5, 10];
#[allow(dead_code)]
const LONGMEM_TURN_CUTOFFS: [usize; 3] = [5, 10, 50];

/// One macro aggregate with the exact accumulated score numerator and count.
///
/// Recall denominators differ by question, and NDCG contains logarithmic
/// discounts, so the macro numerator is necessarily represented as `f64`.
/// `from_scores` is the only constructor used by the scorers and rejects
/// empty, non-finite, and out-of-range values.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[allow(dead_code)]
pub struct MetricValue {
    pub value: f64,
    pub numerator: f64,
    pub denominator: u64,
}

#[allow(dead_code)]
impl MetricValue {
    pub fn from_scores(scores: &[f64]) -> Result<Self> {
        ensure!(!scores.is_empty(), "metric denominator must be positive");
        ensure!(
            scores
                .iter()
                .all(|score| score.is_finite() && (0.0..=1.0).contains(score)),
            "metric inputs must be finite values in [0, 1]"
        );

        let numerator = scores.iter().sum::<f64>();
        let denominator = u64::try_from(scores.len())?;
        let value = numerator / denominator as f64;
        let metric = Self {
            value,
            numerator,
            denominator,
        };
        metric.validate()?;
        Ok(metric)
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.denominator > 0, "metric denominator must be positive");
        ensure!(
            self.numerator.is_finite() && (0.0..=self.denominator as f64).contains(&self.numerator),
            "metric numerator must be finite and within its denominator"
        );
        ensure!(
            self.value.is_finite() && (0.0..=1.0).contains(&self.value),
            "metric value must be finite and in [0, 1]"
        );
        Ok(())
    }
}

impl Serialize for MetricValue {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        let mut state = serializer.serialize_struct("MetricValue", 3)?;
        state.serialize_field("value", &self.value)?;
        state.serialize_field("numerator", &self.numerator)?;
        state.serialize_field("denominator", &self.denominator)?;
        state.end()
    }
}

/// Dataset-native metrics for one variant and one exact ranking granularity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[allow(dead_code)]
pub struct RetrievalMetrics {
    pub dataset: DatasetKind,
    pub granularity: Granularity,
    pub variant: Variant,
    pub overall: BTreeMap<String, MetricValue>,
    pub slices: BTreeMap<String, BTreeMap<String, MetricValue>>,
    pub exclusions: Vec<String>,
}

/// Scorer input binding one question's independent gold views to one ranking.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub struct RetrievalMetricInput {
    pub question_id: String,
    pub category: Option<u32>,
    pub question_type: Option<String>,
    pub caption_evidence: bool,
    pub session_gold_ids: Vec<String>,
    pub turn_gold_ids: Vec<String>,
    pub ranking: Ranking,
}

/// Legacy millipoint evidence recall retained until Task 12 removes the old
/// harness. New reports use the unrounded macro score internally.
pub fn recall_at_k(gold: &[impl AsRef<str>], hits: &[impl AsRef<str>], k: usize) -> u32 {
    let gold = gold.iter().map(AsRef::as_ref).collect::<BTreeSet<_>>();
    if gold.is_empty() {
        return 0;
    }
    let hits = hits
        .iter()
        .take(k)
        .map(AsRef::as_ref)
        .collect::<BTreeSet<_>>();
    let matched = gold.intersection(&hits).count();
    ((matched * 1000) / gold.len()) as u32
}

/// Binary diagnostic: every unique gold occurrence is present in the first k
/// unique ranked positions.
#[allow(dead_code)]
pub fn recall_all_at_k(gold: &[impl AsRef<str>], hits: &[impl AsRef<str>], k: usize) -> f64 {
    let gold = gold.iter().map(AsRef::as_ref).collect::<BTreeSet<_>>();
    if gold.is_empty() {
        return 0.0;
    }
    let hits = hits
        .iter()
        .take(k)
        .map(AsRef::as_ref)
        .collect::<BTreeSet<_>>();
    f64::from(gold.is_subset(&hits))
}

/// Binary diagnostic: at least one unique gold occurrence is present in the
/// first k unique ranked positions.
#[allow(dead_code)]
pub fn recall_any_at_k(gold: &[impl AsRef<str>], hits: &[impl AsRef<str>], k: usize) -> f64 {
    let gold = gold.iter().map(AsRef::as_ref).collect::<BTreeSet<_>>();
    if gold.is_empty() {
        return 0.0;
    }
    let hits = hits
        .iter()
        .take(k)
        .map(AsRef::as_ref)
        .collect::<BTreeSet<_>>();
    f64::from(!gold.is_disjoint(&hits))
}

/// Binary-relevance NDCG with the ideal denominator computed from the number
/// of unique gold occurrences that can fit in k positions.
#[allow(dead_code)]
pub fn ndcg_at_k(gold: &[impl AsRef<str>], hits: &[impl AsRef<str>], k: usize) -> f64 {
    let gold = gold.iter().map(AsRef::as_ref).collect::<BTreeSet<_>>();
    if gold.is_empty() || k == 0 {
        return 0.0;
    }

    let mut seen = BTreeSet::new();
    let dcg = hits
        .iter()
        .take(k)
        .enumerate()
        .filter_map(|(index, hit)| {
            let hit = hit.as_ref();
            (seen.insert(hit) && gold.contains(hit)).then(|| discount(index))
        })
        .sum::<f64>();
    let ideal_dcg = (0..gold.len().min(k)).map(discount).sum::<f64>();
    dcg / ideal_dcg
}

/// Score eligible LoCoMo evidence at turn granularity only.
#[allow(dead_code)]
pub fn score_locomo_retrieval(
    inputs: &[RetrievalMetricInput],
    exclusions: Vec<String>,
) -> Result<RetrievalMetrics> {
    let (variant, granularity) = validate_inputs(inputs, 10, &exclusions)?;
    ensure!(
        granularity == Granularity::Turn,
        "LoCoMo evidence must be scored at turn granularity"
    );
    for input in inputs {
        ensure!(
            input.category.is_some(),
            "{} has no category",
            input.question_id
        );
        validate_gold(input, &input.turn_gold_ids, "evidence")?;
    }

    let overall_inputs = inputs.iter().collect::<Vec<_>>();
    let overall = locomo_metric_map(&overall_inputs)?;
    let mut grouped = BTreeMap::<String, Vec<&RetrievalMetricInput>>::new();
    for input in inputs {
        grouped
            .entry(format!(
                "category:{}",
                input.category.expect("validated category")
            ))
            .or_default()
            .push(input);
        grouped
            .entry(format!("caption_evidence:{}", input.caption_evidence))
            .or_default()
            .push(input);
    }
    let slices = grouped
        .into_iter()
        .map(|(name, slice)| Ok((name, locomo_metric_map(&slice)?)))
        .collect::<Result<_>>()?;

    Ok(RetrievalMetrics {
        dataset: DatasetKind::Locomo,
        granularity,
        variant,
        overall,
        slices,
        exclusions,
    })
}

/// Score LongMemEval using only the gold view matching the immutable ranking's
/// declared granularity. Session and turn gold are never used as fallbacks.
#[allow(dead_code)]
pub fn score_longmemeval_retrieval(
    inputs: &[RetrievalMetricInput],
    exclusions: Vec<String>,
) -> Result<RetrievalMetrics> {
    let first = inputs
        .first()
        .ok_or_else(|| anyhow::anyhow!("metric denominator must be positive"))?;
    let cutoffs = match first.ranking.granularity {
        Granularity::Session => LONGMEM_SESSION_CUTOFFS.as_slice(),
        Granularity::Turn => LONGMEM_TURN_CUTOFFS.as_slice(),
    };
    let (variant, granularity) = validate_inputs(
        inputs,
        *cutoffs.last().expect("fixed nonempty cutoffs"),
        &exclusions,
    )?;
    for input in inputs {
        let question_type = input
            .question_type
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{} has no question type", input.question_id))?;
        ensure!(
            !question_type.contains('\n'),
            "{} has an invalid question type",
            input.question_id
        );
        validate_gold(input, longmem_gold(input, granularity), "LongMemEval")?;
    }

    let overall_inputs = inputs.iter().collect::<Vec<_>>();
    let overall = longmem_metric_map(&overall_inputs, granularity, cutoffs)?;
    let mut grouped = BTreeMap::<String, Vec<&RetrievalMetricInput>>::new();
    for input in inputs {
        grouped
            .entry(format!(
                "question_type:{}",
                input.question_type.as_deref().expect("validated type")
            ))
            .or_default()
            .push(input);
    }
    let slices = grouped
        .into_iter()
        .map(|(name, slice)| Ok((name, longmem_metric_map(&slice, granularity, cutoffs)?)))
        .collect::<Result<_>>()?;

    Ok(RetrievalMetrics {
        dataset: DatasetKind::LongMemEval,
        granularity,
        variant,
        overall,
        slices,
        exclusions,
    })
}

#[allow(dead_code)]
fn locomo_metric_map(inputs: &[&RetrievalMetricInput]) -> Result<BTreeMap<String, MetricValue>> {
    let mut metrics = BTreeMap::new();
    for k in LOCOMO_CUTOFFS {
        let recall = inputs
            .iter()
            .map(|input| {
                let input = *input;
                evidence_recall_at_k(&input.turn_gold_ids, &hit_ids(input), k)
            })
            .collect::<Vec<_>>();
        let all = inputs
            .iter()
            .map(|input| {
                let input = *input;
                recall_all_at_k(&input.turn_gold_ids, &hit_ids(input), k)
            })
            .collect::<Vec<_>>();
        metrics.insert(
            format!("evidence_recall_at_{k}"),
            MetricValue::from_scores(&recall)?,
        );
        metrics.insert(
            format!("all_evidence_hit_at_{k}"),
            MetricValue::from_scores(&all)?,
        );
    }
    Ok(metrics)
}

#[allow(dead_code)]
fn longmem_metric_map(
    inputs: &[&RetrievalMetricInput],
    granularity: Granularity,
    cutoffs: &[usize],
) -> Result<BTreeMap<String, MetricValue>> {
    let mut metrics = BTreeMap::new();
    for &k in cutoffs {
        let all = inputs
            .iter()
            .map(|input| {
                let input = *input;
                recall_all_at_k(longmem_gold(input, granularity), &hit_ids(input), k)
            })
            .collect::<Vec<_>>();
        let any = inputs
            .iter()
            .map(|input| {
                let input = *input;
                recall_any_at_k(longmem_gold(input, granularity), &hit_ids(input), k)
            })
            .collect::<Vec<_>>();
        let ndcg = inputs
            .iter()
            .map(|input| {
                let input = *input;
                ndcg_at_k(longmem_gold(input, granularity), &hit_ids(input), k)
            })
            .collect::<Vec<_>>();
        metrics.insert(
            format!("recall_all_at_{k}"),
            MetricValue::from_scores(&all)?,
        );
        metrics.insert(
            format!("recall_any_at_{k}"),
            MetricValue::from_scores(&any)?,
        );
        metrics.insert(format!("ndcg_at_{k}"), MetricValue::from_scores(&ndcg)?);
    }
    Ok(metrics)
}

#[allow(dead_code)]
fn validate_inputs(
    inputs: &[RetrievalMetricInput],
    required_k: usize,
    exclusions: &[String],
) -> Result<(Variant, Granularity)> {
    let first = inputs
        .first()
        .ok_or_else(|| anyhow::anyhow!("metric denominator must be positive"))?;
    let mut question_ids = BTreeSet::new();
    for exclusion in exclusions {
        ensure!(!exclusion.is_empty(), "metric exclusions must not be empty");
    }
    ensure!(
        exclusions.iter().collect::<BTreeSet<_>>().len() == exclusions.len(),
        "metric exclusions must be unique"
    );

    for input in inputs {
        ensure!(
            !input.question_id.is_empty(),
            "question ID must not be empty"
        );
        ensure!(
            question_ids.insert(input.question_id.as_str()),
            "duplicate metric question {}",
            input.question_id
        );
        ensure!(
            input.ranking.variant == first.ranking.variant,
            "mixed retrieval variants are not one aggregate"
        );
        ensure!(
            input.ranking.granularity == first.ranking.granularity,
            "mixed ranking granularities are not one aggregate"
        );
        ensure!(
            input.ranking.k >= required_k,
            "{} ranking declares k={} but metrics require k={required_k}",
            input.question_id,
            input.ranking.k
        );
        ensure!(
            input.ranking.hits.len() <= input.ranking.k,
            "{} ranking contains more hits than its declared k",
            input.question_id
        );
        let mut ranked_ids = BTreeSet::new();
        for hit in &input.ranking.hits {
            ensure!(
                !hit.occurrence_id.is_empty(),
                "{} ranking contains an empty occurrence ID",
                input.question_id
            );
            ensure!(
                hit.score.is_finite(),
                "{} ranking contains a non-finite score",
                input.question_id
            );
            ensure!(
                ranked_ids.insert(hit.occurrence_id.as_str()),
                "{} ranking contains duplicate occurrence {}",
                input.question_id,
                hit.occurrence_id
            );
        }
    }
    Ok((first.ranking.variant, first.ranking.granularity))
}

#[allow(dead_code)]
fn validate_gold(input: &RetrievalMetricInput, gold: &[String], label: &str) -> Result<()> {
    ensure!(
        !gold.is_empty(),
        "{} has an empty {label} denominator",
        input.question_id
    );
    ensure!(
        gold.iter().all(|id| !id.is_empty()),
        "{} has an empty {label} occurrence ID",
        input.question_id
    );
    ensure!(
        gold.iter().collect::<BTreeSet<_>>().len() == gold.len(),
        "{} has duplicate {label} occurrence IDs",
        input.question_id
    );
    Ok(())
}

#[allow(dead_code)]
fn longmem_gold(input: &RetrievalMetricInput, granularity: Granularity) -> &[String] {
    match granularity {
        Granularity::Session => &input.session_gold_ids,
        Granularity::Turn => &input.turn_gold_ids,
    }
}

#[allow(dead_code)]
fn hit_ids(input: &RetrievalMetricInput) -> Vec<&str> {
    input
        .ranking
        .hits
        .iter()
        .map(|hit| hit.occurrence_id.as_str())
        .collect()
}

#[allow(dead_code)]
fn evidence_recall_at_k(gold: &[impl AsRef<str>], hits: &[impl AsRef<str>], k: usize) -> f64 {
    let gold = gold.iter().map(AsRef::as_ref).collect::<BTreeSet<_>>();
    if gold.is_empty() {
        return 0.0;
    }
    let hits = hits
        .iter()
        .take(k)
        .map(AsRef::as_ref)
        .collect::<BTreeSet<_>>();
    gold.intersection(&hits).count() as f64 / gold.len() as f64
}

#[allow(dead_code)]
fn discount(zero_based_rank: usize) -> f64 {
    1.0 / (zero_based_rank as f64 + 2.0).log2()
}

/// `coverage_milli * total = COVERED_WEIGHT * covered + PARTIAL_WEIGHT * partial`.
pub fn coverage_milli(covered: u32, partial: u32, total: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    (covered * super::COVERED_WEIGHT + partial * super::PARTIAL_WEIGHT) / total
}

pub fn graphify_slice<T>(items: &[T], n: usize) -> &[T] {
    let end = n.min(items.len());
    &items[..end]
}
