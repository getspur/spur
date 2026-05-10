//! Worker signal encoding + parsing.
//!
//! Workers emit structured signals as sentinel-fenced JSON inside a beads
//! comment, plus a `signal:<kind>` label. v0a defines the format; v0b adds
//! the `report_signal` MCP tool that produces them and the brain-side
//! consumer. Shipping the parser in v0a locks the format before consumption.
//!
//! See spec §Information Flow → Signal schema.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SENTINEL_PREFIX: &str = "[[spur-signal v1]]";

/// The full worker-signal enum. v0 ships `ScopeDrift` only; future variants
/// land as additional `#[non_exhaustive]` entries.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerSignal {
    ScopeDrift {
        signal_id: Uuid,
        severity: f32,
        reason: String,
        #[serde(default)]
        estimated_subtasks: Option<u8>,
    },
    /// Brain-side detector signal: the worker created or modified a file
    /// that overlaps non-trivially with an already-approved upstream task's
    /// tip. Emitted by `clobber_detector` during `review_task`. May also
    /// be emitted by future worker-side guards.
    PotentialClobber {
        signal_id: Uuid,
        conflicting_task_id: String,
        file: String,
        /// OID at the upstream task's tip where the file content lives.
        upstream_tip: String,
        /// OID at the current worker's tip where the conflicting content lives.
        worker_tip: String,
    },
    /// bd-2m2u Phase 2e — emitted when a task has exhausted its in-engine
    /// auto-retry budget (1 attempt) (Phase 1) and the autonomous-recovery proposer
    /// path is desired in addition to / instead of the brain escalation
    /// continuation (Phase 2d option A). v0 deterministic proposer matches
    /// on this signal and emits `RetryTask` while attempts < MAX_ATTEMPTS.
    RetryExhausted {
        signal_id: Uuid,
        task_id: String,
        attempt: u32,
        last_error: String,
    },
    /// Worker asserts that completing with no file changes is intentional.
    /// The orchestrator uses this as the no-op acknowledgement when branch
    /// finalization observes zero commits and a clean tree.
    MarkNoop { signal_id: Uuid, reason: String },
}

impl WorkerSignal {
    /// Returns the `signal_id` regardless of variant.
    pub fn signal_id(&self) -> Uuid {
        match self {
            WorkerSignal::ScopeDrift { signal_id, .. } => *signal_id,
            WorkerSignal::PotentialClobber { signal_id, .. } => *signal_id,
            WorkerSignal::RetryExhausted { signal_id, .. } => *signal_id,
            WorkerSignal::MarkNoop { signal_id, .. } => *signal_id,
        }
    }

    /// Returns the kind-string used for `signal:<kind>` labels.
    pub fn kind_label(&self) -> &'static str {
        match self {
            WorkerSignal::ScopeDrift { .. } => "scope-drift",
            WorkerSignal::PotentialClobber { .. } => "potential-clobber",
            WorkerSignal::RetryExhausted { .. } => "retry-exhausted",
            WorkerSignal::MarkNoop { .. } => "mark-noop",
        }
    }
}

/// Encode a `WorkerSignal` as a full sentinel comment body ready for
/// `br comments add`.
pub fn encode_comment(signal: &WorkerSignal) -> String {
    let json = serde_json::to_string(signal).expect("WorkerSignal always serializes");
    format!("{SENTINEL_PREFIX}\n{json}")
}

/// Parse a comment body. Returns `None` if the body does not begin with the
/// sentinel prefix. Returns `Some(Err(_))` if the sentinel is present but
/// the JSON is malformed or the variant is unknown.
pub fn parse_comment(body: &str) -> Option<Result<WorkerSignal, ParseError>> {
    let rest = body.trim_start().strip_prefix(SENTINEL_PREFIX)?;
    let json = rest.trim_start();
    Some(serde_json::from_str(json).map_err(ParseError::Json))
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("sentinel JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_parse_round_trips() {
        let sig = WorkerSignal::ScopeDrift {
            signal_id: Uuid::nil(),
            severity: 0.82,
            reason: "auth refactor pulls in token store".to_string(),
            estimated_subtasks: Some(3),
        };
        let body = encode_comment(&sig);
        assert!(body.starts_with(SENTINEL_PREFIX));
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(parsed, sig);
    }

    #[test]
    fn parse_returns_none_for_non_sentinel_comment() {
        let got = parse_comment("ordinary human comment");
        assert!(got.is_none());
    }

    #[test]
    fn parse_returns_err_for_malformed_sentinel() {
        let body = format!("{SENTINEL_PREFIX}\nnot json");
        let got = parse_comment(&body).unwrap();
        assert!(got.is_err());
    }

    #[test]
    fn parse_tolerates_leading_whitespace() {
        let sig = WorkerSignal::ScopeDrift {
            signal_id: Uuid::nil(),
            severity: 0.1,
            reason: "r".into(),
            estimated_subtasks: None,
        };
        let body = format!("   \n  {}", encode_comment(&sig));
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(parsed, sig);
    }

    #[test]
    fn potential_clobber_round_trips_and_has_label() {
        let sig = WorkerSignal::PotentialClobber {
            signal_id: Uuid::nil(),
            conflicting_task_id: "task-1".to_string(),
            file: "crates/spur-tui/src/foo.rs".to_string(),
            upstream_tip: "abc123".to_string(),
            worker_tip: "def456".to_string(),
        };
        let body = encode_comment(&sig);
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(parsed, sig);
        assert_eq!(sig.kind_label(), "potential-clobber");
        assert_eq!(sig.signal_id(), Uuid::nil());
    }

    #[test]
    fn signal_id_accessor_returns_value() {
        let id = Uuid::new_v4();
        let sig = WorkerSignal::ScopeDrift {
            signal_id: id,
            severity: 0.1,
            reason: "r".into(),
            estimated_subtasks: None,
        };
        assert_eq!(sig.signal_id(), id);
        assert_eq!(sig.kind_label(), "scope-drift");
    }
}
