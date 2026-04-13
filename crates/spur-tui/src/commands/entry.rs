use crate::action::Action;
use serde_json::Value;

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
}

/// How a selected `CommandEntry` should be executed.
#[derive(Debug, Clone)]
pub enum Dispatch {
    /// Fire an `Action` directly, close the popup, do not send a message.
    SpurLocal(Action),
    /// Send the normalized text as a `ContentBlock::Text` to the current agent.
    /// `normalized` is the bare form with leading slash (e.g. "/help").
    PromptText { normalized: String },
    /// Invoke the kiro vendor extension `_kiro.dev/commands/execute`.
    KiroExecute { command: String, args: Value },
}
