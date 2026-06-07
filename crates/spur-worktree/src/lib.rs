#![expect(
    clippy::doc_markdown,
    reason = "legacy worktree docs include type and field identifiers that need a dedicated markdown pass"
)]
#![expect(
    clippy::get_unwrap,
    reason = "legacy test helper style indexes known-present active worktree entries"
)]
#![expect(
    clippy::manual_let_else,
    reason = "legacy git blob listing keeps fallible git calls in match form"
)]
#![expect(
    clippy::single_match_else,
    reason = "legacy worktree command paths keep success and failure branches grouped"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy worktree code predates the current to_owned style lint"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy worktree error strings use pre-inline format argument style"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy worktree modules import extension traits by name for readability"
)]

pub mod artifact;
pub mod git_blob_store;
pub mod manager;

pub use manager::{MergeResult, WorktreeInfo, WorktreeManager};
