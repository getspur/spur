//! Direct-linkage adapter to the `beads_rust` 0.2.1 crate.
//!
//! See `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md`
//! for the full design.

pub mod adapter;
pub mod backoff;
pub mod init;
pub mod metrics;
pub mod reader_pool;
pub mod snapshot;

pub use adapter::{AdapterConfig, BeadsCrateAdapter};
