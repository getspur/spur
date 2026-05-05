//! Direct-linkage adapter to the `beads_rust` 0.2.1 crate.
//!
//! See `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md`
//! for the full design.

pub mod metrics;
// pub mod backoff;       // Task 3
pub mod reader_pool;
// pub mod init;          // Task 5/6/7/8
pub mod snapshot; // Task 11
