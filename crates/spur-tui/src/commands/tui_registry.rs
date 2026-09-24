//! TUI-owned `CommandRegistry` newtype.
//!
//! Wraps the neutral `registry::CommandRegistry` (whose constructors
//! *require* an injected [`LocalLayer`]) and installs the TUI's own
//! `SpurLocalSource::layer()` on every constructor path, so no TUI code
//! can build a registry without `/clear` and the other meta-commands.
//! All existing `CommandRegistry::{new, default, from_configs}` call
//! sites compile unchanged and behave identically to before the
//! injection split. Spec 2026-09-23 §3.2.

use std::ops::{Deref, DerefMut};

use super::spur_local::SpurLocalSource;
use spur_acp::AgentConfig;
use spur_commands::registry;

/// Merged slash-command registry for the TUI: the neutral registry plus
/// the spur-local meta-command layer.
pub struct CommandRegistry(registry::CommandRegistry);

impl CommandRegistry {
    /// Build a registry with the full spur-local meta-command layer
    /// installed.
    pub fn new() -> Self {
        Self(registry::CommandRegistry::new(SpurLocalSource::layer()))
    }

    /// Build a registry pre-populated with static commands from `configs`
    /// plus the full spur-local meta-command layer.
    pub fn from_configs(configs: &[AgentConfig]) -> Self {
        Self(registry::CommandRegistry::from_configs(
            configs,
            SpurLocalSource::layer(),
        ))
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for CommandRegistry {
    type Target = registry::CommandRegistry;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for CommandRegistry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
