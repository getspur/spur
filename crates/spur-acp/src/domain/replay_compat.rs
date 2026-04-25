use serde::{Deserialize, Serialize};

use super::events::SpurEventBody;

/// Replay-compatible body that captures unknown variants without failing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReplayBody {
    Known(SpurEventBody),
    Unknown(serde_json::Value),
}

impl ReplayBody {
    pub fn as_known(&self) -> Option<&SpurEventBody> {
        match self {
            ReplayBody::Known(b) => Some(b),
            ReplayBody::Unknown(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_variant_deserializes_as_known() {
        let json = r#"{"WorkerHeartbeat":{"brain_session_id":"bs","executor_id":"ex","worker_ts":null}}"#;
        let body: ReplayBody = serde_json::from_str(json).unwrap();
        assert!(body.as_known().is_some());
    }

    #[test]
    fn unknown_variant_deserializes_as_unknown_not_error() {
        let json = r#"{"FutureVariantThatDoesNotExist":{"x":1}}"#;
        let body: ReplayBody = serde_json::from_str(json).unwrap();
        assert!(body.as_known().is_none());
    }
}
