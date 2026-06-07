use serde::{Deserialize, Serialize};

use super::events::SpurEventBody;

/// Replay-compatible body that captures unknown variants without failing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[expect(
    clippy::large_enum_variant,
    reason = "replay compatibility keeps the known event body inline for serialization shape"
)]
pub enum ReplayBody {
    Known(SpurEventBody),
    Unknown(serde_json::Value),
}

impl ReplayBody {
    pub fn as_known(&self) -> Option<&SpurEventBody> {
        match self {
            Self::Known(b) => Some(b),
            Self::Unknown(_) => None,
        }
    }

    /// Returns the raw JSON value for unknown variants. Useful for replay
    /// tooling that wants to log or pass through unrecognized payloads
    /// without manual `match` boilerplate.
    pub fn as_unknown(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(v) => Some(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_variant_deserializes_as_known() {
        let json =
            r#"{"WorkerHeartbeat":{"brain_session_id":"bs","executor_id":"ex","worker_ts":null}}"#;
        let body: ReplayBody = serde_json::from_str(json).unwrap();
        assert!(body.as_known().is_some());
    }

    #[test]
    fn unknown_variant_deserializes_as_unknown_not_error() {
        let json = r#"{"FutureVariantThatDoesNotExist":{"x":1}}"#;
        let body: ReplayBody = serde_json::from_str(json).unwrap();
        assert!(body.as_known().is_none());
    }

    #[test]
    fn unknown_variant_round_trips_preserving_payload() {
        let original_json = r#"{"FutureVariantV2":{"new_field":42,"nested":{"x":"y"}}}"#;
        let body: ReplayBody = serde_json::from_str(original_json).unwrap();
        let value = body.as_unknown().expect("should be unknown");
        // Re-serialize and confirm the inner JSON survives untouched.
        let re_emitted = serde_json::to_string(value).unwrap();
        let original_value: serde_json::Value = serde_json::from_str(original_json).unwrap();
        let re_emitted_value: serde_json::Value = serde_json::from_str(&re_emitted).unwrap();
        assert_eq!(re_emitted_value, original_value);
    }
}
