pub mod event_funnel;
pub mod event_sink;
pub mod lineage;
pub(crate) mod notification_drain;
pub mod orchestrator;
mod review_sink;
pub mod skip_perm;
pub mod spur_ext_interp;

pub use spur_acp::{Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role};
pub use lineage::{
    Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode, ReviewRequest,
};
pub use orchestrator::{review_dispatcher_loop, BrainSession, InteractiveInput, Orchestrator, RunOpts, RunResult};
pub use orchestrator::test_support;
pub use review_sink::{ReviewSink, ReviewSinkError};
