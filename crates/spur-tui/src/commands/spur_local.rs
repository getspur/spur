//! Static registry of spur-local slash commands available in every session.
//!
//! # Taxonomy
//!
//! Slash commands fall into two categories:
//!
//! - **Meta commands** (this file): operate on spur's view / session
//!   lifecycle. Examples: `/clear`, `/sessions`, `/help`, `/quit`,
//!   `/cost`, `/vim`. They are **client-owned**: spur
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
use crate::action::{Action, IssueAction, ViewId};
use spur_commands::registry::LocalLayer;

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
    ///   `Arc<Theme>`). `submit_shell::route` always intercepts it
    ///   ahead of the registry, but listing it here prevents collisions
    ///   if an agent ever advertises a `/theme` command and a future
    ///   route wires it through the registry path.
    /// - `/notebook` — owned by the TUI daemon control path. It must never
    ///   be forwarded to the brain because the brain's MCP config is fixed
    ///   for the life of the session.
    /// - `/configure` — owned by the TUI because it mutates SPUR's own
    ///   worker configuration rather than prompting the current brain.
    /// - `/brain` / `/brains` — client-owned multi-brain Scope A hot-swap;
    ///   never forwarded to the agent as prompt text.
    pub fn exclusive_names() -> &'static [&'static str] {
        &["clear", "theme", "notebook", "configure", "brain", "brains"]
    }

    pub fn entries() -> Vec<CommandEntry> {
        vec![
            CommandEntry {
                name: "help".into(),
                description: "Show spur keybindings".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "help".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "clear".into(),
                description: "Close the current session and start fresh".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "clear".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "sessions".into(),
                description: "Open session picker".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "sessions".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "brain".into(),
                description: "Switch brain type or open brain picker".into(),
                hint: Some("[name]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "brain".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "brains".into(),
                description: "List brain-capable agents".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "brains".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "cost".into(),
                description: "Show current session cost".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "cost".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "quit".into(),
                description: "Quit spur".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "quit".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "vim".into(),
                description: "Toggle vim / emacs input mode".into(),
                hint: Some("[Alt+I]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::Local { name: "vim".into() },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "issues".into(),
                description: "Refresh issue list from tracker".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "issues".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "theme".into(),
                description: "Pick, switch, or reload the active TUI theme".into(),
                hint: Some("[name|reload]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "theme".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "notebook".into(),
                description: "Open, create, or close the SPUR notebook".into(),
                hint: Some("[path|new|close]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "notebook".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "configure".into(),
                description: "Open settings browser".into(),
                hint: Some("[section|agent-name]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "configure".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "sprints".into(),
                description: "Open sprint plan browser".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "sprints".into(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "explore".into(),
                description: "Browse and adopt ecosystem skills & agent personas".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::Local {
                    name: "explore".into(),
                },
                arg_picker_spec: None,
            },
        ]
    }

    /// The TUI's meta-command layer, injected into the registry by the
    /// `commands::CommandRegistry` newtype constructors. Entries carry a
    /// neutral `Dispatch::Local { name }` pointing back at their own
    /// catalog name; the frontend resolves the name to an `Action` at the
    /// boundary via [`local_dispatch`].
    pub fn layer() -> LocalLayer {
        LocalLayer {
            entries: Self::entries(),
            exclusive_names: Self::exclusive_names()
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        }
    }
}

/// Resolve a frontend-owned meta-command name (plus optional trimmed arg)
/// to the `Action` the TUI fires for it. This is the static table behind
/// every `Dispatch::Local { name }` in the registry layer and every local
/// interception in `submit_shell` — the single place mapping neutral
/// names back to TUI actions.
///
/// Serves every [`SpurLocalSource::entries`] name plus the submit-router
/// interceptions that are not popup entries (`work`, `issue show <id>`).
/// Unknown names (or unusable args for arg-requiring names) resolve to
/// `None`.
pub fn local_dispatch(name: &str, arg: Option<&str>) -> Option<Action> {
    let arg = arg.map(str::trim).filter(|arg| !arg.is_empty());
    match name {
        "help" => Some(Action::ShowHelp),
        "clear" => Some(Action::ClearSession),
        "sessions" => Some(Action::RequestSessions),
        "brain" => Some(Action::BrainCommand {
            arg: arg.unwrap_or_default().to_string(),
        }),
        "brains" => Some(Action::ListBrains),
        "cost" => Some(Action::ShowSessionCost),
        "quit" => Some(Action::Quit),
        "vim" => Some(Action::ToggleVimMode),
        "issues" => Some(Action::RefreshIssues),
        "theme" => Some(Action::ThemeCommand {
            arg: arg.unwrap_or_default().to_string(),
        }),
        "notebook" => Some(Action::NotebookCommand {
            arg: arg.unwrap_or_default().to_string(),
        }),
        "configure" => Some(Action::NavigateTo(ViewId::AgentConfigBrowser {
            preselect: arg.map(str::to_string),
        })),
        "sprints" => Some(Action::NavigateTo(ViewId::PlanBrowser)),
        "explore" => Some(Action::NavigateTo(ViewId::ExploreBrowser)),
        // Submit-router-only names (not popup entries).
        "work" => arg.map(|id| Action::Issue(IssueAction::WorkOn { id: id.to_string() })),
        "issue" => match arg? {
            // `issue show <id>` → issue ViewDetail action.
            rest if rest.starts_with("show") => {
                let id = rest.strip_prefix("show")?.trim();
                (!id.is_empty())
                    .then(|| Action::Issue(IssueAction::ViewDetail { id: id.to_string() }))
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_configure_describes_settings_browser() {
        let entry = SpurLocalSource::entries()
            .into_iter()
            .find(|entry| entry.name == "configure")
            .expect("configure command entry should exist");

        assert_eq!(entry.description, "Open settings browser");
        assert_eq!(entry.hint.as_deref(), Some("[section|agent-name]"));
    }

    #[test]
    fn slash_explore_routes_to_navigate() {
        let entry = SpurLocalSource::entries()
            .into_iter()
            .find(|entry| entry.name == "explore")
            .expect("explore command entry should exist");

        match &entry.dispatch {
            Dispatch::Local { name } => assert_eq!(name, "explore"),
            other => panic!("expected /explore to carry a local dispatch, got {other:?}"),
        }
        match local_dispatch("explore", None).expect("explore resolves") {
            Action::NavigateTo(ViewId::ExploreBrowser) => {}
            other => panic!("expected /explore to navigate to ExploreBrowser, got {other:?}"),
        }
    }

    #[test]
    fn layer_carries_catalog_and_exclusive_names() {
        let layer = SpurLocalSource::layer();
        assert_eq!(layer.entries.len(), SpurLocalSource::entries().len());
        assert_eq!(
            layer.exclusive_names.len(),
            SpurLocalSource::exclusive_names().len()
        );
        for name in SpurLocalSource::exclusive_names() {
            assert!(layer.exclusive_names.iter().any(|n| n == name));
        }
    }
}
