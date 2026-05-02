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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionState {
    AwaitingReview,
    Failed,
    Cancelled,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpicCompletionOutcome {
    AllApproved,
    TerminalWithFailures,
}

/// Optional fields propagated from a completed delegation into the beads audit
/// comment. Bundled to keep completion plumbing signatures manageable and to
/// localize future field additions to one struct.
#[derive(Debug, Default, Clone)]
pub struct CompletionAuditFields {
    /// Worker's git branch name for the delegation. Carried verbatim from
    /// `DelegationResult.worker_branch` (NOT the materializer's clipped copy)
    /// so operators see the original branch in audit comments.
    pub worker_branch: Option<String>,
    /// Human-visible summary line. For non-Superseded paths this is the
    /// materializer's CLIPPED summary (post `clip_with_ellipsis`). For
    /// Superseded — where the materializer is bypassed — it falls back to
    /// the unclipped `DelegationResult.summary`.
    pub result_summary: Option<String>,
    /// `Some(_)` when `OutcomeMaterializer::materialize` succeeded, formatted
    /// `spur://outcome/{brain_session}/{delegation}/{attempt}`. Operators
    /// extract this URI and resolve via `fetch_outcome_artifact` to inspect
    /// the full delegation result. `None` for the Superseded path (the
    /// materializer is skipped to avoid emitting stale-attempt artifacts).
    pub artifact_uri: Option<String>,
    /// HEAD of the worker worktree immediately after overlay cherry-picks
    /// (post-T9 wiring, dispatch-time). Used for forensics and downstream
    /// range computation. None for legacy or pre-overlay dispatches.
    pub dispatched_base_oid: Option<String>,
    /// Repository root used to validate `dispatched_base_oid..worker_branch`.
    /// This is emission-only context and is not serialized into the sentinel.
    pub repo_root: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpDescription {
    pub kind: String,
    pub issue_id: String,
    #[serde(default)]
    pub depends_on_id: Option<String>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AuditSentinelKind {
    PlanSubmit {
        plan_id: String,
        epic_issue_id: String,
        task_ids: Vec<String>,
        #[serde(default)]
        base_snapshot_branch: Option<String>,
        #[serde(default)]
        base_snapshot_oid: Option<String>,
        #[serde(default)]
        execution_mode: Option<String>,
        #[serde(default)]
        brain_session_id: Option<String>,
    },
    TaskSpec {
        task_id: String,
        #[serde(default)]
        context_files: Vec<String>,
    },
    Dispatch {
        delegation_id: String,
        worker: String,
        attempt: u32,
    },
    DispatchOrphanCleared {
        delegation_id: String,
        reason: String,
    },
    Completion {
        delegation_id: String,
        completion_state: CompletionState,
        #[serde(default)]
        superseded: bool,
        #[serde(default)]
        worker_branch: Option<String>,
        #[serde(default)]
        result_summary: Option<String>,
        /// NEW (Phase 3) - `Some(_)` when OutcomeMaterializer succeeded;
        /// carries the OutcomeKey-derived URI. Operators viewing the beads
        /// issue can extract this and resolve via `fetch_outcome_artifact`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_uri: Option<String>,
        /// HEAD of the worker worktree immediately after overlay cherry-picks
        /// (post-T9 wiring, dispatch-time). Used for forensics + downstream
        /// merge_plan/get_task_diff range computation. None for legacy or
        /// pre-overlay dispatches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dispatched_base_oid: Option<String>,
    },
    EpicCompletion {
        outcome: EpicCompletionOutcome,
        plan_id: String,
        epic_id: String,
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
        #[serde(default)]
        rollback_ops_succeeded: Vec<OpDescription>,
        #[serde(default)]
        rollback_ops_failed: Vec<(OpDescription, String)>,
    },
    LateSignal {
        signal_id: String,
        terminal_status: String,
    },
    WorkerMcp {
        delegation_id: String,
        subkind: WorkerMcpSubkind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_issue_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Forward-compat fallback. When a future SPUR release adds a new audit
    /// sentinel variant and emits it into a beads comment, older clients
    /// parsing that comment will deserialize it into `Unknown` instead of
    /// hard-failing with `ParseError::Json`. Callers iterating sentinels
    /// should skip `Unknown`; the encode path must never emit it.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerMcpSubkind {
    Call,
    AuthDenied,
    ScopeViolation,
    UpstreamFailure,
    FlushFailed,
    PmDegraded,
}

impl AuditSentinelKind {
    /// Kebab-case tag matching the serde `kind` field.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::PlanSubmit { .. } => "plan-submit",
            Self::TaskSpec { .. } => "task-spec",
            Self::Dispatch { .. } => "dispatch",
            Self::DispatchOrphanCleared { .. } => "dispatch-orphan-cleared",
            Self::Completion { .. } => "completion",
            Self::EpicCompletion { .. } => "epic-completion",
            Self::Approval { .. } => "approval",
            Self::Rejection { .. } => "rejection",
            Self::Signal { .. } => "signal",
            Self::MutationPlan { .. } => "mutation-plan",
            Self::MutationCommit { .. } => "mutation-commit",
            Self::MutationInvariantViolation { .. } => "mutation-invariant-violation",
            Self::LateSignal { .. } => "late-signal",
            Self::WorkerMcp { .. } => "worker-mcp",
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

pub fn count_worker_commits(
    repo: &std::path::Path,
    base: &str,
    head: &str,
) -> Result<usize, String> {
    let range = format!("{base}..{head}");
    let out = std::process::Command::new("git")
        .args(["rev-list", "--count", &range])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git rev-list failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-list exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|e| format!("parse count: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan_submit() -> AuditSentinelKind {
        AuditSentinelKind::PlanSubmit {
            plan_id: "P1".into(),
            epic_issue_id: "bd-1".into(),
            task_ids: vec!["bd-2".into(), "bd-3".into()],
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            execution_mode: None,
            brain_session_id: None,
        }
    }

    #[test]
    fn epic_completion_variant_round_trips() {
        let kind = AuditSentinelKind::EpicCompletion {
            outcome: EpicCompletionOutcome::AllApproved,
            plan_id: "P1".into(),
            epic_id: "bd-epic-1".into(),
        };
        let encoded = encode_comment(&kind);
        let parsed = parse_comment(&encoded).unwrap().unwrap();
        assert_eq!(parsed, kind);
        assert_eq!(parsed.kind_str(), "epic-completion");
    }

    #[test]
    fn encode_then_parse_round_trips_all_variants() {
        let cases = vec![
            sample_plan_submit(),
            AuditSentinelKind::TaskSpec {
                task_id: "t1".into(),
                context_files: vec!["docs/spec.md".into(), "src/lib.rs".into()],
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-A".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::DispatchOrphanCleared {
                delegation_id: "del-A".into(),
                reason: crate::server::ORPHAN_CLEAR_REASON_RESTART.into(),
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("feat/x".into()),
                result_summary: None,
                artifact_uri: None,
                dispatched_base_oid: None,
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
                rollback_ops_succeeded: Vec::new(),
                rollback_ops_failed: Vec::new(),
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
            AuditSentinelKind::DispatchOrphanCleared {
                delegation_id: "x".into(),
                reason: crate::server::ORPHAN_CLEAR_REASON_RESTART.into(),
            },
            AuditSentinelKind::Completion {
                delegation_id: "x".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: None,
                result_summary: None,
                artifact_uri: None,
                dispatched_base_oid: None,
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
                rollback_ops_succeeded: Vec::new(),
                rollback_ops_failed: Vec::new(),
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
    fn completion_variant_parses_v2_comments_without_artifact_uri() {
        // Audit comments emitted before Phase 3 don't have artifact_uri.
        // Parser must default to None instead of erroring.
        let v2_json = r#"{"kind":"completion","delegation_id":"abc","completion_state":"awaiting_review","superseded":false,"worker_branch":"spur/worker-x","result_summary":"done"}"#;
        let parsed: AuditSentinelKind = serde_json::from_str(v2_json).expect("parse");
        if let AuditSentinelKind::Completion { artifact_uri, .. } = parsed {
            assert!(artifact_uri.is_none());
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn completion_variant_round_trips_artifact_uri() {
        let s = AuditSentinelKind::Completion {
            delegation_id: "abc".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker-x".into()),
            result_summary: Some("done".into()),
            artifact_uri: Some("spur://outcome/aaa/bbb/1".into()),
            dispatched_base_oid: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: AuditSentinelKind = serde_json::from_str(&json).unwrap();
        if let AuditSentinelKind::Completion { artifact_uri, .. } = back {
            assert_eq!(artifact_uri.as_deref(), Some("spur://outcome/aaa/bbb/1"));
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn completion_kind_round_trips_dispatched_base_oid() {
        let kind = AuditSentinelKind::Completion {
            delegation_id: "abc".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker-x".into()),
            result_summary: Some("done".into()),
            artifact_uri: None,
            dispatched_base_oid: Some("oid123".into()),
        };
        let body = encode_comment(&kind);
        let parsed = parse_comment(&body).unwrap().unwrap();
        match parsed {
            AuditSentinelKind::Completion {
                dispatched_base_oid,
                ..
            } => {
                assert_eq!(dispatched_base_oid.as_deref(), Some("oid123"));
            }
            _ => panic!("expected Completion"),
        }
    }

    #[test]
    fn completion_kind_parses_legacy_comment_without_dispatched_base_oid() {
        let body = format!(
            "{SENTINEL_PREFIX}\n{}",
            serde_json::json!({
                "kind": "completion",
                "delegation_id": "abc",
                "completion_state": "awaiting_review",
                "superseded": false,
                "worker_branch": null,
                "result_summary": null,
            })
        );
        let parsed = parse_comment(&body).unwrap().unwrap();
        match parsed {
            AuditSentinelKind::Completion {
                dispatched_base_oid,
                ..
            } => {
                assert!(dispatched_base_oid.is_none());
            }
            _ => panic!("expected Completion"),
        }
    }

    #[test]
    fn completion_variant_omits_artifact_uri_when_none() {
        let s = AuditSentinelKind::Completion {
            delegation_id: "abc".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: None,
            result_summary: None,
            artifact_uri: None,
            dispatched_base_oid: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("artifact_uri"),
            "skip_serializing_if should omit artifact_uri when None; got: {json}"
        );
    }

    #[test]
    fn completion_state_and_dispatch_orphan_cleared_round_trip() {
        let completion = AuditSentinelKind::Completion {
            delegation_id: "del-A".into(),
            completion_state: CompletionState::Superseded,
            superseded: true,
            worker_branch: Some("feat/stale".into()),
            result_summary: Some("late completion ignored".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        };
        let orphan = AuditSentinelKind::DispatchOrphanCleared {
            delegation_id: "del-A".into(),
            reason: crate::server::ORPHAN_CLEAR_REASON_RESTART.into(),
        };

        let completion_body = encode_comment(&completion);
        let orphan_body = encode_comment(&orphan);

        assert_eq!(
            parse_comment(&completion_body).unwrap().unwrap(),
            completion
        );
        assert_eq!(parse_comment(&orphan_body).unwrap().unwrap(), orphan);
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
            rollback_ops_succeeded: vec![super::OpDescription {
                kind: "remove_dependency".into(),
                issue_id: "bd-2".into(),
                depends_on_id: Some("bd-1".into()),
            }],
            rollback_ops_failed: vec![(
                super::OpDescription {
                    kind: "restore_parent_status".into(),
                    issue_id: "bd-1".into(),
                    depends_on_id: None,
                },
                "sqlite busy".into(),
            )],
        };
        let parsed = parse_comment(&encode_comment(&kind)).unwrap().unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn worker_mcp_sentinel_round_trip() {
        let kind = AuditSentinelKind::WorkerMcp {
            delegation_id: "abc-123".into(),
            subkind: WorkerMcpSubkind::Call,
            tool_name: Some("update_issue".into()),
            target_issue_id: Some("bd-1234".into()),
            error: None,
        };
        let encoded = encode_comment(&kind);
        let decoded = parse_comment(&encoded).unwrap().unwrap();
        assert_eq!(decoded, kind);
        assert_eq!(decoded.kind_str(), "worker-mcp");
        assert!(encoded.contains("\"call\""));
        assert!(!encoded.contains("\"Call\""));
    }

    #[test]
    fn worker_mcp_subkind_kebab_case_serialization() {
        let cases = [
            (WorkerMcpSubkind::Call, "call"),
            (WorkerMcpSubkind::AuthDenied, "auth-denied"),
            (WorkerMcpSubkind::ScopeViolation, "scope-violation"),
            (WorkerMcpSubkind::UpstreamFailure, "upstream-failure"),
            (WorkerMcpSubkind::FlushFailed, "flush-failed"),
            (WorkerMcpSubkind::PmDegraded, "pm-degraded"),
        ];
        for (sub, expected) in cases {
            let json = serde_json::to_string(&sub).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
        }
    }
}
