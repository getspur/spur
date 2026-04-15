//! Executor lineage projection.
//!
//! `ExecutorLineage` is a pure event-sourced projection of the `SpurEvent`
//! stream. Feeding the same events in the same order always produces the same
//! state — safe to rebuild from `SessionHistory` replay.

pub mod adapter;
pub mod projection;
pub mod types;

pub use projection::ExecutorLineage;
pub use types::{
    Artifact, Attempt, AttemptStatus, ExecutorId, ExecutorNode, ReviewDecision, ReviewKind,
    ReviewPayload, ReviewRequest,
};
