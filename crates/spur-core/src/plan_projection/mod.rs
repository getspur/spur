//! Durable plan projection built from `PlanSnapshotUpdated` ACP events.
//!
//! This store is intentionally thin: `spur-mcp` owns the durable plan
//! projection from beads, and `spur-core` caches the already-shaped snapshot
//! for consumers like the TUI.

pub mod projection;
pub mod types;

pub use projection::PlanProjectionStore;
pub use types::{TrackedPlan, TrackedTask};
