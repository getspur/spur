//! Serde types for the Jev System One wire protocol.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum number of options accepted by a choice question.
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// Maximum number of ordered levels accepted by a score question.
pub const MAX_SCORE_LEVELS: usize = 10;
/// Default Jev model alias used for requests.
pub const DEFAULT_MODEL: &str = "jev-latest";

/// A request to the Jev System One endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JevRequest {
    /// Content shared by all questions in the request.
    pub state: Value,
    /// Model name or alias to use.
    pub model: String,
    /// Questions keyed by caller-selected identifiers.
    pub questions: BTreeMap<String, Question>,
}

/// One typed question about the request state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Question {
    /// A yes/no question, answered as a probability of true.
    Noul {
        /// What the model should evaluate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<Value>,
        /// Optional definitions of true and false.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// A selection from caller-defined options.
    Choice {
        /// What the model should decide.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<Value>,
        /// Option names and their descriptions.
        criteria: BTreeMap<String, Value>,
    },
    /// A rating against an ordered rubric.
    Score {
        /// What the model should rate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<Value>,
        /// Ordered score-level descriptions, starting at zero.
        criteria: Vec<Value>,
    },
}

/// Optional definitions for the outcomes of a noul question.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoulCriteria {
    /// What counts as true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#true: Option<Value>,
    /// What counts as false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#false: Option<Value>,
}

/// A response from the Jev System One endpoint.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JevResponse {
    /// Exact model version that served the request.
    pub model: String,
    /// Answers keyed by the request's question identifiers.
    pub answers: BTreeMap<String, Answer>,
    /// Token usage reported by the service.
    pub usage: Usage,
}

/// One typed answer returned by Jev.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    /// Probability of a yes or true answer.
    Noul {
        /// Probability in the inclusive range zero through one.
        noul: f64,
    },
    /// Selection from a choice question.
    Choice {
        /// Option with the highest probability.
        choice: String,
        /// Probability of every option, keyed by option name.
        probabilities: BTreeMap<String, f64>,
        /// Confidence in the selected choice.
        confidence: f64,
    },
    /// Rating against a score question's ordered levels.
    Score {
        /// Probability-weighted average of the rubric levels.
        score: f64,
        /// Rubric descriptions keyed by score level.
        legend: BTreeMap<String, Value>,
        /// Probability of every score level.
        probabilities: BTreeMap<String, f64>,
        /// Confidence in the score.
        confidence: f64,
    },
}

/// Token usage for a Jev request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Billable input token count.
    pub input_tokens: u64,
    /// Output token count.
    pub output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::JevResponse;

    #[test]
    fn wire_types_reject_unknown_fields() {
        let raw = serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {"ready": {"type": "noul", "noul": 0.9, "extra": true}},
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });

        let error = serde_json::from_value::<JevResponse>(raw).expect_err("unknown field");
        assert!(error.to_string().contains("unknown field `extra`"));
    }
}
