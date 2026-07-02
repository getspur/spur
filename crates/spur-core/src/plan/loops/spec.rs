use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use crate::plan::labels::AutonomyLevel;

pub const SENTINEL_HEADER: &str = "[[spur-loop v1]]";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopSpec {
    pub loop_id: String,
    pub goal: String,
    #[serde(default)]
    pub pattern: Option<String>,
    pub cadence_secs: u64,
    #[serde(
        serialize_with = "serialize_autonomy",
        deserialize_with = "deserialize_autonomy"
    )]
    pub autonomy: AutonomyLevel,
    pub template: serde_json::Value,
    #[serde(default)]
    pub governors: LoopGovernors,
    #[serde(default)]
    pub escalation: Option<LoopEscalation>,
}

impl LoopSpec {
    pub fn to_sentinel_body(&self) -> String {
        let json = serde_json::to_string(self).expect("LoopSpec always serializes");
        format!("{SENTINEL_HEADER}\n{json}")
    }

    pub fn parse(body: &str) -> Result<Self, ParseError> {
        let rest = body
            .trim_start()
            .strip_prefix(SENTINEL_HEADER)
            .ok_or(ParseError::MissingSentinel)?;
        serde_json::from_str(rest.trim_start()).map_err(ParseError::Json)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LoopGovernors {
    #[serde(default)]
    pub max_cost_micros_per_generation: Option<u64>,
    #[serde(default)]
    pub max_generations_per_day: Option<u32>,
    #[serde(default)]
    pub max_tasks_per_generation: Option<u32>,
    #[serde(default)]
    pub denylist_globs: Vec<String>,
    #[serde(default)]
    pub consecutive_failure_backoff: Option<FailureBackoff>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureBackoff {
    pub k: u32,
    pub factor: u32,
    pub auto_pause_after: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopEscalation {
    pub after_unresolved_generations: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("missing loop sentinel header")]
    MissingSentinel,
    #[error("loop sentinel JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

fn serialize_autonomy<S>(level: &AutonomyLevel, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(match level {
        AutonomyLevel::L1 => "l1",
        AutonomyLevel::L2 => "l2",
        AutonomyLevel::L3 => "l3",
    })
}

fn deserialize_autonomy<'de, D>(deserializer: D) -> Result<AutonomyLevel, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    match value.as_str() {
        "l1" => Ok(AutonomyLevel::L1),
        "l2" => Ok(AutonomyLevel::L2),
        "l3" => Ok(AutonomyLevel::L3),
        _ => Err(serde::de::Error::unknown_variant(
            &value,
            &["l1", "l2", "l3"],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::labels::{
        loop_generation_label, loop_id_label, loop_next_run_label, parse_loop_generation,
        parse_loop_id, parse_loop_next_run,
    };

    #[test]
    fn loop_spec_sentinel_roundtrips() {
        let spec = LoopSpec {
            loop_id: "0f47ac10b58cc4372".into(),
            goal: "Keep CI green".into(),
            pattern: Some("ci-sweeper".into()),
            cadence_secs: 3600,
            autonomy: AutonomyLevel::L1,
            template: serde_json::json!({"tasks": []}),
            governors: LoopGovernors {
                max_cost_micros_per_generation: Some(2_000_000),
                max_generations_per_day: Some(24),
                max_tasks_per_generation: Some(5),
                denylist_globs: vec!["**/auth/**".into()],
                consecutive_failure_backoff: Some(FailureBackoff {
                    k: 2,
                    factor: 2,
                    auto_pause_after: 4,
                }),
            },
            escalation: Some(LoopEscalation {
                after_unresolved_generations: 3,
            }),
        };
        let body = spec.to_sentinel_body();
        assert!(body.starts_with("[[spur-loop v1]]"));
        assert_eq!(LoopSpec::parse(&body).unwrap(), spec);
    }

    #[test]
    fn loop_labels_roundtrip_and_fit_cap() {
        let id_label = loop_id_label("0f47ac10b58cc4372");
        assert!(id_label.len() <= 50);
        assert_eq!(parse_loop_id(&id_label), Some("0f47ac10b58cc4372"));

        let due = loop_next_run_label(1_782_950_000);
        assert_eq!(parse_loop_next_run(&due), Some(1_782_950_000));

        let generation = loop_generation_label(7);
        assert_eq!(parse_loop_generation(&generation), Some(7));
    }
}
