//! DuckLake-backed code context service for external packages.

pub mod abuse;
pub mod catalog;
pub mod jobs;
pub mod knowledge;
#[cfg(feature = "lambda")]
pub mod lambda;
pub mod medallion;
pub mod mcp;
pub mod query;
pub mod translate;
#[cfg(feature = "worker")]
pub mod worker;
