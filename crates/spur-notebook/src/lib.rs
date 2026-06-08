#![expect(
    clippy::derive_partial_eq_without_eq,
    reason = "legacy notebook DTOs derive PartialEq without consistently deriving Eq"
)]
#![expect(
    clippy::doc_markdown,
    reason = "legacy notebook MCP docs contain domain terms that are not consistently backticked yet"
)]
#![expect(
    clippy::elidable_lifetime_names,
    reason = "legacy notebook MCP trait adapters spell explicit lifetimes"
)]
#![expect(
    clippy::future_not_send,
    reason = "legacy notebook MCP transport futures do not require Send in local serving paths"
)]
#![expect(
    clippy::filter_map_bool_then,
    reason = "legacy notebook DAG graph code uses bool::then in filter_map"
)]
#![expect(
    clippy::ignored_unit_patterns,
    reason = "legacy notebook async select branches use wildcard unit patterns"
)]
#![expect(
    clippy::large_futures,
    reason = "legacy notebook MCP server awaits large service futures directly"
)]
#![expect(
    clippy::large_types_passed_by_value,
    reason = "legacy semantic-search cache stores fixed embeddings by value"
)]
#![expect(
    clippy::map_err_ignore,
    reason = "legacy notebook packaging maps errors into domain errors without preserving all sources"
)]
#![expect(
    clippy::single_match_else,
    reason = "legacy notebook MCP stream handling uses match for nontrivial fallback branches"
)]
#![expect(
    clippy::return_and_then,
    reason = "legacy notebook DAG extraction uses and_then chains"
)]
#![expect(
    clippy::match_same_arms,
    reason = "legacy notebook type mapping keeps explicit equivalent cases"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy notebook code has many &str to String conversions pending mechanical cleanup"
)]
#![expect(
    clippy::too_many_arguments,
    reason = "legacy notebook OAuth helper passes explicit connection fields"
)]
#![expect(
    clippy::unneeded_struct_pattern,
    reason = "legacy notebook daemon command matches still spell unit variants as struct patterns"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy notebook media helpers have not all moved to captured format args"
)]
#![expect(
    clippy::unused_async,
    reason = "legacy Tauri and MCP command signatures preserve async ABI shape"
)]
#![expect(
    clippy::unused_self,
    reason = "legacy notebook service methods keep receiver shape for trait/API consistency"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy modules import extension traits by name"
)]
#![expect(
    clippy::use_self,
    reason = "legacy notebook code often spells concrete type names in impl bodies"
)]

pub mod commands;
pub mod connection_secrets;
pub mod connection_store;
pub mod dag;
#[cfg(feature = "datasource-introspect")]
pub mod datasource;
pub mod extension_install;
pub mod mcp;
pub mod open_design;
pub mod recents;
pub mod spur_app;
