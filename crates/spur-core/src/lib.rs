pub mod event_funnel;
pub mod event_sink;
pub mod license_runtime;
pub mod lineage;
pub(crate) mod notification_drain;
pub mod notification_pump;
pub mod orchestrator;
mod review_sink;
pub mod skills;
pub mod skip_perm;
pub mod spur_ext_interp;

pub use lineage::{
    Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode, ReviewRequest,
    WorkerStreamEntry, WorkerStreamKind,
};
pub use orchestrator::test_support;
pub use orchestrator::{
    review_dispatcher_loop, BrainSession, InteractiveInput, Orchestrator, RunOpts, RunResult,
};
pub use review_sink::{ReviewSink, ReviewSinkError};
pub use spur_acp::{
    Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
};
