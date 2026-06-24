//! DuckLake-backed code context service for external packages.

pub mod abuse;
pub mod catalog;
pub mod jobs;
pub mod knowledge;
#[cfg(feature = "lambda")]
pub mod lambda;
pub mod mcp;
pub mod query;
mod s3_credentials;
pub mod translate;
#[cfg(feature = "worker")]
pub mod worker;
