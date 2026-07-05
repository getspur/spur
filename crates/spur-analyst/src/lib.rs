//! Shared Rust query layer over `.spur/analyst.duckdb`.

pub mod api;
pub(crate) mod db;
pub(crate) mod doc_nav;
pub mod embedding;
pub mod mcp;
pub(crate) mod overlay;
pub(crate) mod pack;
pub mod paths;
pub mod search;

pub use api::*;
pub use paths::{
    query_context_paths, query_context_paths_with_conn, query_symbol_risk_community,
    query_symbol_risk_community_with_conn, MAX_CONTEXT_PATHS, MAX_CONTEXT_PATH_HOPS,
    MAX_SYMBOL_RISK_COMMUNITY_IDS,
};
pub use search::{
    context_candidates::{query_context_candidates, query_context_candidates_with_conn},
    graph_candidates::{query_graph_candidates, query_graph_candidates_with_conn},
};
