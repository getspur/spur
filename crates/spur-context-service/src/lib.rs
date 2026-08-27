//! DuckLake-backed code context service for external packages.

#[cfg(feature = "service")]
pub mod abuse;
pub mod api_key_authorizer;
pub mod api_key_cleanup;
pub mod api_keys;
#[cfg(feature = "service")]
mod auth;
#[cfg(feature = "service")]
pub mod catalog;
#[cfg(feature = "service")]
pub mod drainer;
#[cfg(feature = "service")]
pub mod jobs;
#[cfg(feature = "service")]
pub mod knowledge;
#[cfg(feature = "lambda")]
pub mod lambda;
#[cfg(feature = "service")]
pub mod mcp;
#[cfg(feature = "service")]
pub mod medallion;
#[cfg(feature = "service")]
pub mod query;
pub mod serving_registry;
#[cfg(feature = "service")]
pub mod staleness;
#[cfg(feature = "service")]
pub mod translate;
#[cfg(feature = "worker")]
pub mod worker;
