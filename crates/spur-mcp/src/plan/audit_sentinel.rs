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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        result_summary: Option<String>,
    },
    Approval {
        delegation_id: String,
    },
    Rejection {
        delegation_id: String,
        feedback: String,
    },
    Signal {
        signal_id: String,
        #[serde(rename = "signal_kind")]
        kind: String,
        severity: f32,
        reason: String,
    },
    MutationPlan {
        mutation_id: String,
        op: String,
        #[serde(default)]
        trigger_signal_id: Option<String>,
        trigger_task_id: String,
    },
    MutationCommit {
        mutation_id: String,
        children_created: Vec<String>,
    },
    MutationInvariantViolation {
        mutation_id: String,
        violation: String,
        rollback_status: String,
    },
    LateSignal {
        signal_id: String,
        terminal_status: String,
    },
    /// Forward-compat fallback. When a future SPUR release adds a new audit
    /// sentinel variant and emits it into a beads comment, older clients
    /// parsing that comment will deserialize it into `Unknown` instead of
    /// hard-failing with `ParseError::Json`. Callers iterating sentinels
    /// should skip `Unknown`; the encode path must never emit it.
    #[serde(other)]
    Unknown,
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
            Self::Signal { .. } => "signal",
            Self::MutationPlan { .. } => "mutation-plan",
            Self::MutationCommit { .. } => "mutation-commit",
            Self::MutationInvariantViolation { .. } => "mutation-invariant-violation",
            Self::LateSignal { .. } => "late-signal",
            Self::Unknown => "unknown",
        }
    }
}

/// Encode a kind as a full sentinel comment body ready for `br comments add`.
///
/// The `Unknown` forward-compat variant is a parse-only fallback; emitting it
/// would write a `{"kind":"unknown"}` breadcrumb that downstream readers can't
/// interpret. Internal callers only ever construct known variants, so guard
/// the invariant with a debug assertion.
pub fn encode_comment(kind: &AuditSentinelKind) -> String {
    debug_assert!(
        !matches!(kind, AuditSentinelKind::Unknown),
        "encode_comment must not be called with the Unknown forward-compat variant"
    );
    let json = serde_json::to_string(kind).expect("AuditSentinelKind always serializes");
    format!("{SENTINEL_PREFIX}\n{json}")
}

/// Parse a comment body. Returns `None` if the body does not begin with the
/// sentinel prefix. Returns `Some(Err(_))` if the sentinel is present but
/// the JSON is malformed or the variant is unknown.
pub fn parse_comment(body: &str) -> Option<Result<AuditSentinelKind, ParseError>> {
    let rest = body.trim_start().strip_prefix(SENTINEL_PREFIX)?;
    let json = rest.trim_start();
    let result = serde_json::from_str::<AuditSentinelKind>(json).map_err(ParseError::Json);
    if matches!(result, Ok(AuditSentinelKind::Unknown)) {
        tracing::debug!(
            kind = "unknown",
            sentinel_prefix = SENTINEL_PREFIX,
            "parsed future-compat Unknown variant"
        );
    }
    Some(result)
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
                result_summary: None,
            },
            AuditSentinelKind::Approval {
                delegation_id: "del-A".into(),
            },
            AuditSentinelKind::Rejection {
                delegation_id: "del-A".into(),
                feedback: "try again".into(),
            },
            AuditSentinelKind::Signal {
                signal_id: "sig-1".into(),
                kind: "scope-drift".into(),
                severity: 0.82,
                reason: "auth spans 4 subsystems".into(),
            },
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-V".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-1".into()),
                trigger_task_id: "bd-102".into(),
            },
            AuditSentinelKind::MutationCommit {
                mutation_id: "mut-V".into(),
                children_created: vec!["bd-201".into(), "bd-202".into()],
            },
            AuditSentinelKind::MutationInvariantViolation {
                mutation_id: "mut-V".into(),
                violation: "cycle".into(),
                rollback_status: "completed".into(),
            },
            AuditSentinelKind::LateSignal {
                signal_id: "sig-2".into(),
                terminal_status: "approved".into(),
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
    fn parse_unknown_future_variant_returns_unknown_not_error() {
        // Simulate a future SPUR version's comment with a `kind` we don't know
        // about. Before the Unknown forward-compat variant, this hard-fails at
        // ParseError::Json(unknown variant). After: deserializes as Unknown so
        // older clients tolerate a rolling upgrade / rollback.
        let future_comment = format!(
            "{SENTINEL_PREFIX}\n{{\"kind\":\"mutation-applied\",\"mutation_id\":\"uuid-123\"}}"
        );
        let parsed = parse_comment(&future_comment).unwrap();
        assert!(
            parsed.is_ok(),
            "unknown future variants must deserialize as Unknown fallback, got: {parsed:?}"
        );
        assert_eq!(parsed.unwrap(), AuditSentinelKind::Unknown);
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
                result_summary: None,
            },
            AuditSentinelKind::Approval {
                delegation_id: "x".into(),
            },
            AuditSentinelKind::Rejection {
                delegation_id: "x".into(),
                feedback: "f".into(),
            },
            AuditSentinelKind::Signal {
                signal_id: "sig-1".into(),
                kind: "scope-drift".into(),
                severity: 0.82,
                reason: "auth spans 4 subsystems".into(),
            },
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-V".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-1".into()),
                trigger_task_id: "bd-102".into(),
            },
            AuditSentinelKind::MutationCommit {
                mutation_id: "mut-V".into(),
                children_created: vec!["bd-201".into(), "bd-202".into()],
            },
            AuditSentinelKind::MutationInvariantViolation {
                mutation_id: "mut-V".into(),
                violation: "cycle".into(),
                rollback_status: "completed".into(),
            },
            AuditSentinelKind::LateSignal {
                signal_id: "sig-2".into(),
                terminal_status: "approved".into(),
            },
            AuditSentinelKind::Unknown,
        ] {
            let json = serde_json::to_value(&k).unwrap();
            assert_eq!(json["kind"].as_str(), Some(k.kind_str()));
        }
    }

    #[test]
    fn parse_missing_kind_errors() {
        // No `kind` discriminator at all — serde must reject (NOT silently
        // fall back to Unknown). Without the discriminator we cannot tell
        // whether this is a known or future variant.
        let body = format!("{SENTINEL_PREFIX}\n{{\"delegation_id\":\"x\"}}");
        assert!(parse_comment(&body).unwrap().is_err());
    }

    #[test]
    fn parse_null_kind_errors() {
        // `kind: null` is not a string and must not match any tag, including
        // the `#[serde(other)]` Unknown fallback (which requires a string).
        let body = format!("{SENTINEL_PREFIX}\n{{\"kind\":null}}");
        assert!(parse_comment(&body).unwrap().is_err());
    }

    #[test]
    fn parse_empty_after_prefix_errors() {
        // Prefix present, body empty → invalid JSON, must error (not Unknown).
        let body = format!("{SENTINEL_PREFIX}\n");
        assert!(parse_comment(&body).unwrap().is_err());
    }

    #[test]
    fn parse_known_variant_with_extra_field_tolerates() {
        // Forward-compat WITHIN a known variant: if a future SPUR release adds
        // a new optional field to `Approval`, older clients must still parse
        // the known fields cleanly and silently ignore the extras — not fall
        // through to Unknown, not error.
        let body = format!(
            "{SENTINEL_PREFIX}\n{{\"kind\":\"approval\",\"delegation_id\":\"del-A\",\"future_field\":\"ignored\"}}"
        );
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(
            parsed,
            AuditSentinelKind::Approval {
                delegation_id: "del-A".into(),
            }
        );
    }

    #[test]
    fn kind_str_unknown_returns_expected_string() {
        // Locks in the logging bucket name used by the tracing::debug! call
        // in parse_comment. If this changes, update the observability story.
        assert_eq!(AuditSentinelKind::Unknown.kind_str(), "unknown");
    }

    #[test]
    fn signal_variant_round_trips() {
        let kind = AuditSentinelKind::Signal {
            signal_id: "sig-1".into(),
            kind: "scope-drift".into(),
            severity: 0.82,
            reason: "auth spans 4 subsystems".into(),
        };
        let encoded = encode_comment(&kind);
        let parsed = parse_comment(&encoded).unwrap().unwrap();
        assert_eq!(parsed, kind);
        assert_eq!(parsed.kind_str(), "signal");
    }

    #[test]
    fn mutation_plan_and_commit_round_trip() {
        let plan = AuditSentinelKind::MutationPlan {
            mutation_id: "mut-V".into(),
            op: "split".into(),
            trigger_signal_id: Some("sig-1".into()),
            trigger_task_id: "bd-102".into(),
        };
        let parsed = parse_comment(&encode_comment(&plan)).unwrap().unwrap();
        assert_eq!(parsed, plan);

        let commit = AuditSentinelKind::MutationCommit {
            mutation_id: "mut-V".into(),
            children_created: vec!["bd-201".into(), "bd-202".into()],
        };
        let parsed_c = parse_comment(&encode_comment(&commit)).unwrap().unwrap();
        assert_eq!(parsed_c, commit);
    }

    #[test]
    fn late_signal_round_trips() {
        let kind = AuditSentinelKind::LateSignal {
            signal_id: "sig-2".into(),
            terminal_status: "approved".into(),
        };
        let parsed = parse_comment(&encode_comment(&kind)).unwrap().unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn invariant_violation_round_trips() {
        let kind = AuditSentinelKind::MutationInvariantViolation {
            mutation_id: "mut-V".into(),
            violation: "cycle".into(),
            rollback_status: "completed".into(),
        };
        let parsed = parse_comment(&encode_comment(&kind)).unwrap().unwrap();
        assert_eq!(parsed, kind);
    }
}
