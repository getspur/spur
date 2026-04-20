//! `[[spur-audit v1]]` sentinel comment encoder/parser — extends the
//! plan/signals.rs pattern for audit breadcrumbs. SPUR emits one sentinel
//! comment per plan lifecycle event (submit/dispatch/completion/approval/
//! rejection) on the target beads issue, via `br comments add`.
//!
//! `br audit record` is empirically DOA as transport (data field dropped on
//! persist; no CLI to query interactions; not in `br schema all` bundle;
//! undocumented in upstream AGENTS.md). See plan 2026-04-20-adaptive-plan-
//! repair-v0a.md "Review addendum II" for the full justification.

use serde::{Deserialize, Serialize};

pub const SENTINEL_PREFIX: &str = "[[spur-audit v1]]";

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AuditSentinelKind {
    PlanSubmit {
        plan_id: String,
        epic_issue_id: String,
        task_ids: Vec<String>,
    },
    Dispatch {
        delegation_id: String,
        worker: String,
        attempt: u32,
    },
    Completion {
        delegation_id: String,
        #[serde(default)]
        worker_branch: Option<String>,
        #[serde(default)]
        diff_summary: Option<String>,
    },
    Approval {
        delegation_id: String,
    },
    Rejection {
        delegation_id: String,
        feedback: String,
    },
}

impl AuditSentinelKind {
    /// Kebab-case tag matching the serde `kind` field.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::PlanSubmit { .. } => "plan-submit",
            Self::Dispatch { .. } => "dispatch",
            Self::Completion { .. } => "completion",
            Self::Approval { .. } => "approval",
            Self::Rejection { .. } => "rejection",
        }
    }
}

/// Encode a kind as a full sentinel comment body ready for `br comments add`.
pub fn encode_comment(kind: &AuditSentinelKind) -> String {
    let json = serde_json::to_string(kind).expect("AuditSentinelKind always serializes");
    format!("{SENTINEL_PREFIX}\n{json}")
}

/// Parse a comment body. Returns `None` if the body does not begin with the
/// sentinel prefix. Returns `Some(Err(_))` if the sentinel is present but
/// the JSON is malformed or the variant is unknown.
pub fn parse_comment(body: &str) -> Option<Result<AuditSentinelKind, ParseError>> {
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

    fn sample_plan_submit() -> AuditSentinelKind {
        AuditSentinelKind::PlanSubmit {
            plan_id: "P1".into(),
            epic_issue_id: "bd-1".into(),
            task_ids: vec!["bd-2".into(), "bd-3".into()],
        }
    }

    #[test]
    fn encode_then_parse_round_trips_all_variants() {
        let cases = vec![
            sample_plan_submit(),
            AuditSentinelKind::Dispatch {
                delegation_id: "del-A".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                worker_branch: Some("feat/x".into()),
                diff_summary: None,
            },
            AuditSentinelKind::Approval {
                delegation_id: "del-A".into(),
            },
            AuditSentinelKind::Rejection {
                delegation_id: "del-A".into(),
                feedback: "try again".into(),
            },
        ];
        for k in cases {
            let body = encode_comment(&k);
            assert!(body.starts_with(SENTINEL_PREFIX));
            let parsed = parse_comment(&body).unwrap().unwrap();
            assert_eq!(parsed, k);
        }
    }

    #[test]
    fn parse_returns_none_for_non_sentinel_comment() {
        assert!(parse_comment("ordinary human comment").is_none());
    }

    #[test]
    fn parse_returns_err_for_malformed_sentinel() {
        let body = format!("{SENTINEL_PREFIX}\nnot json");
        assert!(parse_comment(&body).unwrap().is_err());
    }

    #[test]
    fn parse_tolerates_leading_whitespace() {
        let k = sample_plan_submit();
        let body = format!("   \n  {}", encode_comment(&k));
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(parsed, k);
    }

    #[test]
    fn kind_str_matches_serde_tag() {
        // Ensure the accessor agrees with the serde serialization.
        for k in [
            sample_plan_submit(),
            AuditSentinelKind::Dispatch {
                delegation_id: "x".into(),
                worker: "y".into(),
                attempt: 0,
            },
            AuditSentinelKind::Completion {
                delegation_id: "x".into(),
                worker_branch: None,
                diff_summary: None,
            },
            AuditSentinelKind::Approval {
                delegation_id: "x".into(),
            },
            AuditSentinelKind::Rejection {
                delegation_id: "x".into(),
                feedback: "f".into(),
            },
        ] {
            let json = serde_json::to_value(&k).unwrap();
            assert_eq!(json["kind"].as_str(), Some(k.kind_str()));
        }
    }
}
