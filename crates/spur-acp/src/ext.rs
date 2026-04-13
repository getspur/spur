//! Vendor-extension method names used across spur.
//!
//! The ACP protocol reserves `_<vendor>.dev/...` methods for out-of-spec
//! features. These constants keep the wire-format strings in one place.

/// Kiro vendor extension — notification: available commands advertised by kiro.
///
/// Payload shape: `{ sessionId: String, availableCommands: [AvailableCommand] }`.
pub const KIRO_COMMANDS_AVAILABLE: &str = "_kiro.dev/commands/available";

/// Kiro vendor extension — request: execute a kiro command.
///
/// Payload shape: `{ sessionId: String, command: String, args: Value }`.
pub const KIRO_COMMANDS_EXECUTE: &str = "_kiro.dev/commands/execute";

/// Spur-synthetic event method used to publish a kiro execute response
/// back into the TUI as an `AgentExtNotification`.
pub const SPUR_KIRO_EXECUTE_RESPONSE: &str = "_spur.dev/kiro/execute/response";
