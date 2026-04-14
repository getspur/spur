//! Vendor-extension method names used across spur.
//!
//! The ACP protocol reserves `_<vendor>.dev/...` methods for out-of-spec
//! features. These constants keep the wire-format strings in one place.

/// Kiro vendor extension — notification: available commands advertised by kiro.
///
/// Payload shape: `{ sessionId: String, availableCommands: [AvailableCommand] }`.
pub const KIRO_COMMANDS_AVAILABLE: &str = "_kiro.dev/commands/available";
