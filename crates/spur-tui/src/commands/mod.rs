pub mod advertised;
pub mod entry;
pub mod fuzzy;
pub mod kiro_skills;
pub mod registry;
pub mod spur_local;
pub mod submit_router;
pub mod tui_registry;

pub use entry::{CommandEntry, CommandSource, Dispatch};
pub use spur_local::{local_dispatch, SpurLocalSource};
pub use tui_registry::CommandRegistry;
