//! Deterministic Jev questions for solver routing.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use thiserror::Error;

pub use crate::snapshot::{CatalogSnapshot, RuleCard};
use crate::wire::{NoulCriteria, Question, MAX_CHOICE_OPTIONS};

/// Errors that prevent construction of a valid routing battery.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BatteryError {
    /// Jev cannot represent a choice with this many rule cards.
    #[error("routing battery has {actual} options, exceeding the maximum of {maximum}")]
    TooManyOptions {
        /// Number of rule cards supplied by the snapshot.
        actual: usize,
        /// Maximum supported by the Jev choice wire type.
        maximum: usize,
    },
}

/// Builds the fixed routing questions from explicit intent, data, and catalog inputs.
///
/// Every question repeats the evidence it asks Jev to interpret so it remains
/// meaningful when evaluated independently of the other questions.
pub fn generate_routing_battery(
    intent: &str,
    data: &Value,
    snapshot: &CatalogSnapshot,
    language_version: u32,
) -> Result<BTreeMap<String, Question>, BatteryError> {
    if snapshot.rules.len() > MAX_CHOICE_OPTIONS {
        return Err(BatteryError::TooManyOptions {
            actual: snapshot.rules.len(),
            maximum: MAX_CHOICE_OPTIONS,
        });
    }

    let data = canonicalize(data);
    let evidence = |question: &str| {
        Some(json!({
            "catalog_language_version": snapshot.language_version,
            "data": data,
            "intent": intent,
            "language_version": language_version,
            "question": question,
        }))
    };

    let route_criteria = snapshot
        .rules
        .iter()
        .map(|card| {
            (
                card.rule_id.clone(),
                json!({
                    "family": card.family,
                    "summary": card.summary,
                }),
            )
        })
        .collect();

    let solve_mode_criteria = BTreeMap::from([
        (
            "synthesize".to_owned(),
            Value::String(
                "Bounded unknowns must be completed under every selected rule.".to_owned(),
            ),
        ),
        (
            "verify".to_owned(),
            Value::String(
                "A complete model is supplied; evaluate every selected rule against it.".to_owned(),
            ),
        ),
    ]);

    Ok(BTreeMap::from([
        (
            "route_rule".to_owned(),
            Question::Choice {
                instructions: evidence("Which solver rule best matches `intent` and `data`?"),
                criteria: route_criteria,
            },
        ),
        (
            "solve_mode".to_owned(),
            Question::Choice {
                instructions: evidence(
                    "Should the solver verify a complete model or synthesize bounded unknowns?",
                ),
                criteria: solve_mode_criteria,
            },
        ),
        (
            "data_complete".to_owned(),
            Question::Noul {
                instructions: evidence(
                    "Is every value required by the matching rule present and concrete in `data`?",
                ),
                criteria: Some(NoulCriteria {
                    r#true: Some(Value::String(
                        "All required subjects, parameters, and scene or facts values are concrete."
                            .to_owned(),
                    )),
                    r#false: Some(Value::String(
                        "A required value is missing, vague, symbolic, or a placeholder.".to_owned(),
                    )),
                }),
            },
        ),
    ]))
}

/// Recursively sorts JSON object keys before rebuilding each object.
///
/// Determinism must not depend on the `serde_json` feature graph.
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect::<BTreeMap<String, Value>>();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        scalar => scalar.clone(),
    }
}
