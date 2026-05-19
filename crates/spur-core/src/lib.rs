pub mod continuation_bridge;
pub use continuation_bridge::{
    new_overflow_buf, report_detached_completion, ContinuationEventSink, OverflowBuf,
};

pub mod delegation_watchdog;
pub mod scheduler;
pub use scheduler::{BrainScheduler, ScheduledAction};

pub mod event_funnel;
pub mod event_replay;
pub mod event_sink;
pub mod license_runtime;
pub mod lineage;
pub(crate) mod notification_drain;
pub mod notification_pump;
pub mod orchestrator;
pub mod peer_mailbox;
pub mod plan_projection;
pub mod project_root;
pub mod retry_loop;
pub mod review_sink;
pub mod session_synopsis;
pub mod skills;
pub mod skip_perm;
pub mod spur_ext_interp;
pub mod upgrade;
pub mod worktree_authority;

pub use lineage::{
    Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode, PeerEdge, PeerEdgeState,
    ReviewRequest, WorkerStreamEntry, WorkerStreamKind,
};
#[cfg(any(test, feature = "test-support"))]
pub use orchestrator::test_support;
pub use orchestrator::{
    review_dispatcher_loop, BrainSession, InteractiveInput, Orchestrator, RunOpts, RunResult,
};
pub use plan_projection::{PlanProjectionStore, TrackedPlan, TrackedTask};
pub use review_sink::{ReviewSink, ReviewSinkError};
pub use session_synopsis::{SessionSynopsis, SessionSynopsisProjection};
pub use spur_acp::{
    Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
};
pub use upgrade::UpgradeBanner;
pub use worktree_authority::{AuthorityConfig, SweepReport, WorktreeAuthority};
