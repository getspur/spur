//! Content-addressed outcome blob storage for SPUR delegations.
//!
//! This crate owns the [`OutcomeStore`] trait and its in-process
//! implementations (`MemoryOutcomeStore`, `FsOutcomeStore`,
//! `MeasuredOutcomeStore`). The `GitBlobOutcomeStore` impl lives in
//! `spur-worktree` because it owns git ref operations.
//!
//! The wire-shape types (`OutcomeKey`, `OutcomeRef`, `BackendTag`)
//! live in `spur-acp::domain::outcome` and will be re-exported below
//! once Task 2 adds them.
//!
//! Spec: `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md` §6.

pub use spur_acp::domain::outcome::{BackendTag, OutcomeBlobKind, OutcomeKey, OutcomeRef};

pub mod fs_store;
pub mod measured;
pub mod memory_store;
#[cfg(any(test, feature = "test-support"))]
pub mod test_helpers;
pub mod trait_def;
pub mod types;

// Re-exports activated as types land in Tasks 3–6.
pub use fs_store::FsOutcomeStore;
pub use measured::MeasuredOutcomeStore;
pub use memory_store::MemoryOutcomeStore;
pub use trait_def::OutcomeStore;
pub use types::{
    ContentType, DeleteNamespaceReport, OutcomeContent, OutcomeMetadata, Section, StoreError,
    SweepReport,
};
