// Neutral command modules moved to `spur-commands` (spec 2026-09-23
// §3.3/§5 C2, C3). Module re-exports keep every existing
// `crate::commands::{advertised,entry,fuzzy,kiro_skills,submit}::…` path
// compiling; the TUI keeps its meta-command catalog (`spur_local`), the
// submit shell over the pure classifier, and the `CommandRegistry`
// newtype.
pub mod spur_local;
pub mod submit_shell;
pub mod tui_registry;

pub use spur_commands::{advertised, entry, fuzzy, kiro_skills, submit};

pub use spur_commands::entry::{CommandEntry, CommandSource, Dispatch};
pub use spur_local::{local_dispatch, SpurLocalSource};
pub use tui_registry::CommandRegistry;
