use crate::action::Action;
use spur_acp::ArgsTemplateKind;

/// An entry displayed in the slash-command popup.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    /// Command name without the leading slash (e.g. "help", "compact").
    pub name: String,
    /// Human-readable description shown beside the name.
    pub description: String,
    /// Optional input placeholder from ACP `UnstructuredCommandInput.hint`.
    pub hint: Option<String>,
    /// Where this command came from.
    pub source: CommandSource,
    /// How to execute it on accept/submit.
    pub dispatch: Dispatch,
    /// If Some, typing `/<name> <arg>` opens an arg picker.
    pub arg_picker_spec: Option<spur_acp::adapter::arg_picker_hint::ArgPickerSpec>,
}

/// Where a `CommandEntry` originates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    /// A spur-local command handled by the TUI.
    Spur,
    /// A command advertised by an ACP agent (or its vendor extension).
    /// `handle` is the lowercase agent identifier used for namespacing
    /// (e.g. "claude", "kiro").
    Agent { handle: String },
    /// Synthesized by spur from an agent's advertised data (e.g.
    /// NewSessionResponse.config_options). Vendor-neutral by allow-list;
    /// see crates/spur-acp/src/adapter/config_options.rs.
    Advertised { handle: String },
}

/// How a selected `CommandEntry` should be executed.
#[derive(Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "transient UI action/payload enums; instances are short-lived and never stored in bulk, boxing would churn every construction site"
)]
pub enum Dispatch {
    /// Fire an `Action` directly, close the popup, do not send a message.
    SpurLocal(Action),
    /// Send the normalized text as a `ContentBlock::Text` to the current agent.
    /// `normalized` is the bare form with leading slash (e.g. "/help").
    PromptText { normalized: String },
    /// Invoke an agent-specific vendor extension RPC.
    VendorExec {
        /// Full wire method (e.g. `"_kiro.dev/commands/execute"`).
        method: String,
        /// The command name (no leading slash).
        command: String,
        /// How to shape rest-of-line text into the RPC args payload.
        args_template: ArgsTemplateKind,
    },
    /// v1: dispatch to ACP `session/set_config_option`. Used by the synthetic
    /// /model and /effort slash commands. The `value` is filled in by the
    /// arg-picker selection (or by the user's typed arg) at submit time —
    /// see InteractiveInput::SetSessionConfigOption (Task 2.14).
    SetSessionConfigOption { config_id: String },
}
