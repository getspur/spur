#![expect(
    clippy::allow_attributes,
    clippy::doc_markdown,
    clippy::elidable_lifetime_names,
    clippy::format_push_string,
    clippy::ignored_unit_patterns,
    clippy::iter_over_hash_type,
    clippy::map_err_ignore,
    clippy::match_same_arms,
    clippy::missing_fields_in_debug,
    clippy::ref_option,
    clippy::ref_patterns,
    clippy::return_and_then,
    clippy::single_match_else,
    clippy::str_to_string,
    clippy::uninlined_format_args,
    clippy::unnecessary_debug_formatting,
    clippy::unnecessary_wraps,
    clippy::unused_trait_names,
    clippy::use_self,
    clippy::useless_let_if_seq,
    clippy::verbose_file_reads,
    reason = "spur-pm has a pre-existing style-lint backlog; keep -D warnings useful for bug-catching lints"
)]

pub mod adapter;
pub mod advanced;
pub mod beads_crate;
mod blocking_pool_probe;
pub mod bv;
pub mod github;
pub mod graph;
pub mod graph_engine;
pub mod ingest;
mod lock_trace;
pub mod pidfile;
pub mod poll_cursor;
pub mod service;
pub mod sync;
pub mod test_workspace;
pub mod types;

pub use adapter::{IssueTracker, PrService};
pub use advanced::{BeadsAdvanced, Comment, CommentId, DependencyCycle, ReadyFilter};
pub use bv::BvAdapter;
pub use github::GitHubAdapter;
pub use poll_cursor::{PollCursor, POLL_FETCH_LIMIT};
pub use service::PmService;
pub use sync::*;
pub use types::*;
