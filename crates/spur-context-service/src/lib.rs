//! DuckLake-backed code context service for external packages.

pub mod abuse;
mod auth;
pub mod catalog;
pub mod drainer;
pub mod jobs;
pub mod knowledge;
#[cfg(feature = "lambda")]
pub mod lambda;
pub mod mcp;
pub mod medallion;
pub mod query;
pub mod staleness;
pub mod translate;
#[cfg(feature = "worker")]
pub mod worker;
