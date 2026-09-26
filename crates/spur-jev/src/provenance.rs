//! Redacted provenance for gated Jev compilation decisions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    gate::{DecisionReview, Gate},
    wire::{Answer, JevResponse},
};

/// Serializable evidence describing the model decisions behind a request.
///
/// Transport credentials and request headers are deliberately absent from this
/// type, so serializing provenance cannot disclose API key material.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JevProvenance {
    /// Exact model version returned by the Jev service.
    pub served_model_version: String,
    /// Per-question typed answers returned by the service.
    pub answers: BTreeMap<String, Answer>,
    /// Choice/score confidences and explicitly gated noul values.
    pub confidences: BTreeMap<String, f64>,
    /// Minimum value across the gating decisions.
    pub weakest: f64,
    /// Gate outcome derived from the same decision values.
    pub gate: Gate,
}

impl JevProvenance {
    /// Captures redacted provenance from a served response and its review.
    #[must_use]
    pub fn from_response(response: &JevResponse, review: &DecisionReview) -> Self {
        Self {
            served_model_version: response.model.clone(),
            answers: response.answers.clone(),
            confidences: review.per_decision.clone(),
            weakest: review.weakest,
            gate: review.gate.clone(),
        }
    }
}
