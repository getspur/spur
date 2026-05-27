use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use spur_mcp::plan::audit_sentinel::{AuditSentinelKind, CompletionState};
use spur_mcp::plan::projector::project_status_from_audits;
use spur_mcp::plan::PlanTaskStatus;

/// Transition rules extracted from `project_status_from_audits`:
///
/// - Start in `Pending` with no remembered completion summary.
/// - `Dispatch` always projects to `Dispatched { delegation_id }`.
/// - `Completion` first updates the remembered summary when
///   `result_summary` is `Some`, then projects by `completion_state`:
///   `AwaitingReview` -> `AwaitingReview { summary }`, `Failed` ->
///   `Failed { error }`, `Cancelled` -> `Cancelled { reason }`, and
///   `Superseded` -> `Superseded { mutation_id: "unknown", by: [] }`.
/// - Failed and cancelled completions use the remembered summary as their
///   error/reason, falling back to `"worker failed"` or `"worker cancelled"`
///   only when no summary has ever been remembered.
/// - `Approval` projects to `Approved { summary }` using the remembered
///   summary.
/// - `Rejection` projects to `Rejected { feedback: Some(feedback) }`.
/// - `ReviewFeedback` and `RetryRequested` project to `Pending`; they do not
///   clear the remembered summary.
/// - `EscalationRequested` projects to
///   `EscalatedToBrain { last_error }`.
/// - `Signal` only affects status when `signal_kind` is
///   `integration-conflict` or `integration_conflict`; then `reason` is parsed
///   as JSON `{ dep_task_id: String, files: Vec<String> }`, falling back to
///   `dep_task_id: "unknown"` and `files: []` on parse failure.
/// - All other audit variants are status noise and leave both projected status
///   and remembered summary unchanged.
///
/// Shrinkage scope: the generated transition enum covers every audit shape
/// that can affect `PlanTaskStatus`, plus representative ignored noise. It
/// maps to real `AuditSentinelKind` values at runtime so failures shrink to
/// compact domain events instead of full sentinel payloads.

#[derive(Debug, Clone)]
struct ModelState {
    expected: ExpectedStatus,
    summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpectedStatus {
    Pending,
    Dispatched {
        delegation_id: String,
    },
    AwaitingReview {
        summary: Option<String>,
    },
    Approved {
        summary: Option<String>,
    },
    Rejected {
        feedback: Option<String>,
    },
    Failed {
        error: String,
    },
    Cancelled {
        reason: String,
    },
    Superseded {
        mutation_id: String,
        by: Vec<String>,
    },
    BlockedOnSetupConflict {
        dep_task_id: String,
        files: Vec<String>,
    },
    EscalatedToBrain {
        last_error: String,
    },
}

#[derive(Debug, Clone)]
enum AuditStep {
    Dispatch {
        delegation_id: String,
    },
    Completion {
        delegation_id: String,
        state: CompletionShape,
        result_summary: Option<String>,
    },
    Approval {
        delegation_id: String,
    },
    Rejection {
        delegation_id: String,
        feedback: String,
    },
    ReviewFeedback {
        delegation_id: String,
    },
    RetryRequested {
        delegation_id: String,
    },
    EscalationRequested {
        delegation_id: Option<String>,
        last_error: String,
    },
    IntegrationConflictSignal {
        kind: IntegrationConflictKind,
        reason: ConflictReasonShape,
    },
    Noise,
}

#[derive(Debug, Clone, Copy)]
enum CompletionShape {
    AwaitingReview,
    Failed,
    Cancelled,
    Superseded,
}

#[derive(Debug, Clone, Copy)]
enum IntegrationConflictKind {
    Kebab,
    Snake,
}

#[derive(Debug, Clone)]
enum ConflictReasonShape {
    Valid {
        dep_task_id: String,
        files: Vec<String>,
    },
    Invalid,
}

impl ModelState {
    fn initial() -> Self {
        Self {
            expected: ExpectedStatus::Pending,
            summary: None,
        }
    }
}

impl AuditStep {
    fn apply_to_model(&self, mut model: ModelState) -> ModelState {
        match self {
            Self::Dispatch { delegation_id } => {
                model.expected = ExpectedStatus::Dispatched {
                    delegation_id: delegation_id.clone(),
                };
            }
            Self::Completion {
                state,
                result_summary,
                ..
            } => {
                if let Some(summary) = result_summary {
                    model.summary = Some(summary.clone());
                }
                model.expected = match state {
                    CompletionShape::AwaitingReview => ExpectedStatus::AwaitingReview {
                        summary: model.summary.clone(),
                    },
                    CompletionShape::Failed => ExpectedStatus::Failed {
                        error: model
                            .summary
                            .clone()
                            .unwrap_or_else(|| "worker failed".to_string()),
                    },
                    CompletionShape::Cancelled => ExpectedStatus::Cancelled {
                        reason: model
                            .summary
                            .clone()
                            .unwrap_or_else(|| "worker cancelled".to_string()),
                    },
                    CompletionShape::Superseded => ExpectedStatus::Superseded {
                        mutation_id: "unknown".to_string(),
                        by: Vec::new(),
                    },
                };
            }
            Self::Approval { .. } => {
                model.expected = ExpectedStatus::Approved {
                    summary: model.summary.clone(),
                };
            }
            Self::Rejection { feedback, .. } => {
                model.expected = ExpectedStatus::Rejected {
                    feedback: Some(feedback.clone()),
                };
            }
            Self::ReviewFeedback { .. } | Self::RetryRequested { .. } => {
                model.expected = ExpectedStatus::Pending;
            }
            Self::EscalationRequested { last_error, .. } => {
                model.expected = ExpectedStatus::EscalatedToBrain {
                    last_error: last_error.clone(),
                };
            }
            Self::IntegrationConflictSignal { reason, .. } => {
                let (dep_task_id, files) = match reason {
                    ConflictReasonShape::Valid { dep_task_id, files } => {
                        (dep_task_id.clone(), files.clone())
                    }
                    ConflictReasonShape::Invalid => ("unknown".to_string(), Vec::new()),
                };
                model.expected = ExpectedStatus::BlockedOnSetupConflict { dep_task_id, files };
            }
            Self::Noise => {}
        }
        model
    }

    fn to_audit(&self) -> AuditSentinelKind {
        match self {
            Self::Dispatch { delegation_id } => AuditSentinelKind::Dispatch {
                delegation_id: delegation_id.clone(),
                worker: "codex".to_string(),
                attempt: 1,
            },
            Self::Completion {
                delegation_id,
                state,
                result_summary,
            } => {
                let completion_state = match state {
                    CompletionShape::AwaitingReview => CompletionState::AwaitingReview,
                    CompletionShape::Failed => CompletionState::Failed,
                    CompletionShape::Cancelled => CompletionState::Cancelled,
                    CompletionShape::Superseded => CompletionState::Superseded,
                };
                AuditSentinelKind::Completion {
                    delegation_id: delegation_id.clone(),
                    completion_state,
                    superseded: matches!(completion_state, CompletionState::Superseded),
                    worker_branch: matches!(completion_state, CompletionState::AwaitingReview)
                        .then(|| "spur/worker-projector-sm".to_string()),
                    result_summary: result_summary.clone(),
                    artifact_uri: None,
                    dispatched_base_oid: None,
                }
            }
            Self::Approval { delegation_id } => AuditSentinelKind::Approval {
                delegation_id: delegation_id.clone(),
            },
            Self::Rejection {
                delegation_id,
                feedback,
            } => AuditSentinelKind::Rejection {
                delegation_id: delegation_id.clone(),
                feedback: feedback.clone(),
            },
            Self::ReviewFeedback { delegation_id } => AuditSentinelKind::ReviewFeedback {
                delegation_id: delegation_id.clone(),
                attempt: 1,
                feedback: "review feedback".to_string(),
                worker_branch: None,
                summary: None,
                reuse_prior_worktree: None,
            },
            Self::RetryRequested { delegation_id } => AuditSentinelKind::RetryRequested {
                delegation_id: delegation_id.clone(),
                attempt: 1,
                error: "retry requested".to_string(),
                worker_branch: None,
                amended_prompt_summary: None,
            },
            Self::EscalationRequested {
                delegation_id,
                last_error,
            } => AuditSentinelKind::EscalationRequested {
                plan_id: "P1".to_string(),
                task_id: "T1".to_string(),
                attempt: 1,
                last_error: last_error.clone(),
                worker_branch: None,
                delegation_id: delegation_id.clone(),
            },
            Self::IntegrationConflictSignal { kind, reason } => {
                let reason = match reason {
                    ConflictReasonShape::Valid { dep_task_id, files } => serde_json::json!({
                        "dep_task_id": dep_task_id,
                        "files": files,
                    })
                    .to_string(),
                    ConflictReasonShape::Invalid => "not-json".to_string(),
                };
                AuditSentinelKind::Signal {
                    signal_id: "sig-1".to_string(),
                    delegation_id: "del-conflict".to_string(),
                    kind: match kind {
                        IntegrationConflictKind::Kebab => "integration-conflict",
                        IntegrationConflictKind::Snake => "integration_conflict",
                    }
                    .to_string(),
                    severity: 0.9,
                    reason,
                }
            }
            Self::Noise => AuditSentinelKind::TaskTransition {
                plan_id: "P1".to_string(),
                task_id: "T1".to_string(),
                from_status: "pending".to_string(),
                to_status: "ready".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone)]
struct ProjectorSut {
    audits: Vec<AuditSentinelKind>,
}

struct AuditReference;
struct ProjectorStateMachine;

impl ReferenceStateMachine for AuditReference {
    type State = ModelState;
    type Transition = AuditStep;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(ModelState::initial()).boxed()
    }

    fn transitions(_state: &Self::State) -> BoxedStrategy<Self::Transition> {
        let files = proptest::collection::vec(
            prop_oneof![
                Just("crates/spur-mcp/src/plan/projector.rs".to_string()),
                Just("Cargo.toml".to_string()),
            ],
            0..=3,
        );
        let conflict_reason = prop_oneof![
            (Just("dep-task".to_string()), files)
                .prop_map(|(dep_task_id, files)| ConflictReasonShape::Valid { dep_task_id, files }),
            Just(ConflictReasonShape::Invalid),
        ];

        prop_oneof![
            arb_delegation_id().prop_map(|delegation_id| AuditStep::Dispatch { delegation_id }),
            (arb_delegation_id(), arb_completion_shape(), arb_summary()).prop_map(
                |(delegation_id, state, result_summary)| AuditStep::Completion {
                    delegation_id,
                    state,
                    result_summary,
                }
            ),
            arb_delegation_id().prop_map(|delegation_id| AuditStep::Approval { delegation_id }),
            (arb_delegation_id(), arb_feedback()).prop_map(|(delegation_id, feedback)| {
                AuditStep::Rejection {
                    delegation_id,
                    feedback,
                }
            }),
            arb_delegation_id()
                .prop_map(|delegation_id| AuditStep::ReviewFeedback { delegation_id }),
            arb_delegation_id()
                .prop_map(|delegation_id| AuditStep::RetryRequested { delegation_id }),
            (proptest::option::of(arb_delegation_id()), arb_last_error()).prop_map(
                |(delegation_id, last_error)| AuditStep::EscalationRequested {
                    delegation_id,
                    last_error,
                }
            ),
            (
                prop_oneof![
                    Just(IntegrationConflictKind::Kebab),
                    Just(IntegrationConflictKind::Snake),
                ],
                conflict_reason,
            )
                .prop_map(|(kind, reason)| AuditStep::IntegrationConflictSignal { kind, reason }),
            Just(AuditStep::Noise),
        ]
        .boxed()
    }

    fn apply(state: Self::State, transition: &Self::Transition) -> Self::State {
        transition.apply_to_model(state)
    }
}

fn arb_delegation_id() -> impl Strategy<Value = String> {
    proptest::string::string_regex("del-[a-z0-9]{1,4}").expect("valid delegation id regex")
}

fn arb_completion_shape() -> impl Strategy<Value = CompletionShape> {
    prop_oneof![
        Just(CompletionShape::AwaitingReview),
        Just(CompletionShape::Failed),
        Just(CompletionShape::Cancelled),
        Just(CompletionShape::Superseded),
    ]
}

fn arb_summary() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        Just(Some("summary".to_string())),
        Just(Some("later summary".to_string())),
    ]
}

fn arb_feedback() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("needs changes".to_string()),
        Just("missing tests".to_string()),
    ]
}

fn arb_last_error() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("worker failed".to_string()),
        Just("retry budget exhausted".to_string()),
    ]
}

impl StateMachineTest for ProjectorStateMachine {
    type SystemUnderTest = ProjectorSut;
    type Reference = AuditReference;

    fn init_test(_ref_state: &ModelState) -> Self::SystemUnderTest {
        ProjectorSut { audits: Vec::new() }
    }

    fn apply(
        mut state: Self::SystemUnderTest,
        ref_state: &ModelState,
        transition: AuditStep,
    ) -> Self::SystemUnderTest {
        state.audits.push(transition.to_audit());
        assert_eq!(
            status_shape(project_status_from_audits(&state.audits)),
            ref_state.expected,
            "projector diverged after transition {transition:?}; audits={:?}",
            state.audits
        );
        state
    }

    fn check_invariants(state: &Self::SystemUnderTest, ref_state: &ModelState) {
        assert_eq!(
            status_shape(project_status_from_audits(&state.audits)),
            ref_state.expected
        );
    }
}

fn status_shape(status: PlanTaskStatus) -> ExpectedStatus {
    match status {
        PlanTaskStatus::Pending => ExpectedStatus::Pending,
        PlanTaskStatus::Ready => panic!("project_status_from_audits must not produce Ready"),
        PlanTaskStatus::Dispatched { delegation_id } => {
            ExpectedStatus::Dispatched { delegation_id }
        }
        PlanTaskStatus::AwaitingReview { summary } => ExpectedStatus::AwaitingReview { summary },
        PlanTaskStatus::Approved { summary } => ExpectedStatus::Approved { summary },
        PlanTaskStatus::Rejected { feedback } => ExpectedStatus::Rejected { feedback },
        PlanTaskStatus::Failed { error } => ExpectedStatus::Failed { error },
        PlanTaskStatus::Cancelled { reason } => ExpectedStatus::Cancelled { reason },
        PlanTaskStatus::Superseded { mutation_id, by } => {
            ExpectedStatus::Superseded { mutation_id, by }
        }
        PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files } => {
            ExpectedStatus::BlockedOnSetupConflict { dep_task_id, files }
        }
        PlanTaskStatus::EscalatedToBrain { last_error } => {
            ExpectedStatus::EscalatedToBrain { last_error }
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        .. ProptestConfig::default()
    })]

    #[test]
    fn audit_sequence_projection_matches_reference_model(sequential 1..80 => ProjectorStateMachine);
}
