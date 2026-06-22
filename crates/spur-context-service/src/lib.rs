//! DuckLake-backed code context service for external packages.

pub mod catalog;
pub mod knowledge;
#[cfg(feature = "lambda")]
pub mod lambda;
pub mod mcp;
pub mod query;
pub mod translate;
