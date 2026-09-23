//! Neutral slash-command model shared by every spur frontend (spec
//! 2026-09-23 §3): the entry/dispatch model, the merged command registry
//! with an injected frontend [`LocalLayer`](registry::LocalLayer),
//! advertised-entry synthesis, fuzzy ranking, Kiro skill discovery, and
//! the pure entry builder.
//!
//! The crate is frontend-agnostic by construction: it depends only on
//! `spur-acp` plus matching utilities. The TUI installs its meta-command
//! layer through `LocalLayer` injection (§3.2) and resolves
//! [`Dispatch::Local`](entry::Dispatch::Local) names at its own boundary.

#![expect(
    clippy::doc_markdown,
    reason = "modules moved verbatim from spur-tui (spec 2026-09-23 §3.3); their docs are not backticked yet"
)]
#![expect(
    clippy::str_to_string,
    reason = "modules moved verbatim from spur-tui (spec 2026-09-23 §3.3) keep their &str-to-String conversions pending mechanical cleanup"
)]
#![expect(
    clippy::match_same_arms,
    reason = "modules moved verbatim from spur-tui (spec 2026-09-23 §3.3) keep duplicated match arms for readability"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "modules moved verbatim from spur-tui (spec 2026-09-23 §3.3) keep their existing format! arg style"
)]

pub mod advertised;
pub mod entry;
pub mod entry_builder;
pub mod fuzzy;
pub mod kiro_skills;
pub mod registry;

pub use entry::{CommandEntry, CommandSource, Dispatch};
pub use registry::{CommandRegistry, LocalLayer};
