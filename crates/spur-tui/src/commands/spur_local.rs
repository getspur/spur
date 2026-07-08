//! Static registry of spur-local slash commands available in every session.
//!
//! # Taxonomy
//!
//! Slash commands fall into two categories:
//!
//! - **Meta commands** (this file): operate on spur's view / session
//!   lifecycle. Examples: `/clear`, `/sessions`, `/help`, `/quit`,
//!   `/mode`, `/cost`, `/vim`. They are **client-owned**: spur
//!   intercepts them before they ever reach the agent. Agent-advertised
//!   entries with the same name are **shadowed** by these spur-local
//!   entries (see `CommandRegistry::ensure_cache`).
//!
//! - **Conversational commands** (declared in an agent's config under
//!   `[commands.static]` or advertised at runtime via
//!   `_<agent>.dev/commands/available`): affect the brain's reasoning
//!   or context. Examples: `/compact`, `/model`, `/undo`, `/review`.
//!   These flow through `Dispatch::PromptText` or
//!   `Dispatch::VendorExec` and are handled by the agent.
//!
//! When a name collides between categories, meta wins. This is why
//! kiro's advertised `/clear` (which does NOT match the client's
//! expected behavior — see the 2026-04-15 brainstorm docs) is hidden
//! from the popup in favor of spur's uniform retire+respawn handler.

use super::entry::{CommandEntry, CommandSource, Dispatch};
use crate::action::Action;

/// Static registry of spur-local slash commands available in every session.
pub struct SpurLocalSource;

impl SpurLocalSource {
    /// Names of spur-local meta-commands that **exclusively** own their
    /// command name: any agent-advertised entry with the same name is
    /// suppressed from the registry. Entries NOT in this set can still
    /// coexist with agent entries of the same name (collision-display
    /// logic applies).
    ///
    /// - `/clear` — must behave identically across every brain kind and
    ///   forwarding it to the agent produces inconsistent or broken
    ///   results.
    /// - `/theme` — owned end-to-end by the TUI (palette + active
    ///   `Arc<Theme>`). `submit_router::route` always intercepts it
    ///   ahead of the registry, but listing it here prevents collisions
    ///   if an agent ever advertises a `/theme` command and a future
    ///   route wires it through the registry path.
    /// - `/notebook` — owned by the TUI daemon control path. It must never
    ///   be forwarded to the brain because the brain's MCP config is fixed
    ///   for the life of the session.
    /// - `/configure` — owned by the TUI because it mutates SPUR's own
    ///   worker configuration rather than prompting the current brain.
    pub fn exclusive_names() -> &'static [&'static str] {
        &["clear", "theme", "notebook", "configure"]
    }

    pub fn entries() -> Vec<CommandEntry> {
        vec![
            CommandEntry {
                name: "help".into(),
                description: "Show spur keybindings".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ShowHelp),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "clear".into(),
                description: "Close the current session and start fresh".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ClearSession),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "mode".into(),
                description: "Toggle Claude session mode (plan / default)".into(),
                hint: Some("[plan|default]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::TogglePlanMode),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "sessions".into(),
                description: "Open session picker".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::RequestSessions),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "cost".into(),
                description: "Show current session cost".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ShowSessionCost),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "quit".into(),
                description: "Quit spur".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::Quit),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "vim".into(),
                description: "Toggle vim / emacs input mode".into(),
                hint: Some("[Alt+I]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ToggleVimMode),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "issues".into(),
                description: "Refresh issue list from tracker".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::RefreshIssues),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "theme".into(),
                description: "Pick, switch, or reload the active TUI theme".into(),
                hint: Some("[name|reload]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ThemeCommand { arg: String::new() }),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "notebook".into(),
                description: "Open, create, or close the SPUR notebook".into(),
                hint: Some("[path|new|close]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::NotebookCommand { arg: String::new() }),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "configure".into(),
                description: "Open agent settings browser".into(),
                hint: Some("[agent-name]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::NavigateTo(
                    crate::action::ViewId::AgentConfigBrowser { preselect: None },
                )),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "sprints".into(),
                description: "Open sprint plan browser".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::NavigateTo(
                    crate::action::ViewId::PlanBrowser,
                )),
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "explore".into(),
                description: "Browse and adopt ecosystem skills & agent personas".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::NavigateTo(
                    crate::action::ViewId::ExploreBrowser,
                )),
                arg_picker_spec: None,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ViewId;

    #[test]
    fn slash_explore_routes_to_navigate() {
        let entry = SpurLocalSource::entries()
            .into_iter()
            .find(|entry| entry.name == "explore")
            .expect("explore command entry should exist");

        match entry.dispatch {
            Dispatch::SpurLocal(Action::NavigateTo(ViewId::ExploreBrowser)) => {}
            other => panic!("expected /explore to navigate to ExploreBrowser, got {other:?}"),
        }
    }
}
