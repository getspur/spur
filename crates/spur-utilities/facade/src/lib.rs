//! Facade for the `spur-utilities` crate family (spec §2.3).
//!
//! Re-exports only: [`commands`] (`spur-commands`) and [`mentions`]
//! (`spur-mentions`). No types, no logic, and the facade never enables the
//! `code` feature of `spur-mentions`.

pub use spur_commands as commands;
pub use spur_mentions as mentions;
