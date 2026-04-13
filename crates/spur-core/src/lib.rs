pub mod lineage;
pub mod orchestrator;
mod review_sink;

pub use spur_acp::{Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role};
pub use lineage::{
    Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode, ReviewRequest,
};
pub use orchestrator::{review_dispatcher_loop, BrainSession, InteractiveInput, Orchestrator, RunOpts, RunResult};
pub use review_sink::{ReviewSink, ReviewSinkError};
