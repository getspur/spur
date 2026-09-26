//! Weakest-link confidence gating for Jev decisions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::wire::Answer;

/// Minimum confidence required for every gating decision.
pub const GATE_THRESHOLD: f64 = 0.60;

/// The decision that determines a review's weakest-link result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionConfidence {
    /// Question identifier from the Jev battery.
    pub question: String,
    /// Confidence or gating noul value used by the gate.
    pub value: f64,
}

/// Details returned when at least one decision is below the gate threshold.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AmbiguityReport {
    /// Every decision value considered by the gate, in stable question order.
    pub per_decision: BTreeMap<String, f64>,
    /// The deterministic minimum across `per_decision`.
    pub weakest_link: DecisionConfidence,
    /// Threshold the weakest link failed to meet.
    pub threshold: f64,
}

/// Whether compilation may proceed after decision review.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", content = "ambiguity", rename_all = "snake_case")]
pub enum Gate {
    /// Every decision met [`GATE_THRESHOLD`].
    Open,
    /// One or more decisions were too uncertain.
    Blocked(AmbiguityReport),
}

/// Stable weakest-link review of the decisions that shape a compiled request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReview {
    /// Every decision value considered by the gate, in stable question order.
    pub per_decision: BTreeMap<String, f64>,
    /// Minimum value across `per_decision`.
    pub weakest: f64,
    /// Question and value that produced `weakest`.
    pub weakest_link: DecisionConfidence,
    /// Result of comparing `weakest` with [`GATE_THRESHOLD`].
    pub gate: Gate,
}

impl DecisionReview {
    /// Reviews explicit question/value pairs.
    ///
    /// Duplicate question identifiers use the final supplied value. An empty
    /// set is conservatively blocked as a missing decision.
    #[must_use]
    pub fn from_confidences(confidences: &[(&str, f64)]) -> Self {
        let per_decision = confidences
            .iter()
            .map(|(question, value)| ((*question).to_owned(), normalize(*value)))
            .collect();
        Self::new(per_decision)
    }

    /// Reviews every choice/score confidence plus only the named gating nouls.
    ///
    /// This keeps meta self-report nouls out of the safety gate while ensuring
    /// no closed-set decision confidence can be skipped.
    #[must_use]
    pub fn from_answers(answers: &BTreeMap<String, Answer>, gating_nouls: &[&str]) -> Self {
        let gating_nouls = gating_nouls.iter().copied().collect::<BTreeSet<_>>();
        let per_decision = answers
            .iter()
            .filter_map(|(question, answer)| {
                let value = match answer {
                    Answer::Choice { confidence, .. } | Answer::Score { confidence, .. } => {
                        Some(*confidence)
                    }
                    Answer::Noul { noul } if gating_nouls.contains(question.as_str()) => {
                        Some(*noul)
                    }
                    Answer::Noul { .. } => None,
                };
                value.map(|value| (question.clone(), normalize(value)))
            })
            .collect();
        Self::new(per_decision)
    }

    fn new(per_decision: BTreeMap<String, f64>) -> Self {
        let weakest_link = per_decision
            .iter()
            .min_by(|left, right| left.1.total_cmp(right.1))
            .map_or_else(
                || DecisionConfidence {
                    question: "<missing>".to_owned(),
                    value: 0.0,
                },
                |(question, value)| DecisionConfidence {
                    question: question.clone(),
                    value: *value,
                },
            );
        let weakest = weakest_link.value;
        let gate = if weakest >= GATE_THRESHOLD {
            Gate::Open
        } else {
            Gate::Blocked(AmbiguityReport {
                per_decision: per_decision.clone(),
                weakest_link: weakest_link.clone(),
                threshold: GATE_THRESHOLD,
            })
        };

        Self {
            per_decision,
            weakest,
            weakest_link,
            gate,
        }
    }
}

fn normalize(value: f64) -> f64 {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        value
    } else {
        0.0
    }
}
