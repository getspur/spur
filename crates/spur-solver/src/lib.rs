//! Constraint model-finding for SPUR coding agents.

#![expect(
    clippy::map_err_ignore,
    reason = "family compilers replace conversion errors with domain-specific rule messages"
)]

pub mod cache;
pub mod constraint_spec;
pub mod encode;
pub mod mcp;
pub mod persist;
pub mod process;
pub mod rules;
pub mod service;
pub mod session;
pub mod smt_gate;
pub mod types;
