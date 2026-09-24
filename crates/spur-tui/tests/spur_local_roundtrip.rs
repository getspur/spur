//! Round-trip parity for the TUI-local meta-command catalog.
//!
//! Every `SpurLocalSource` entry name must resolve through
//! `local_dispatch`, and every `local_dispatch` result must trace back to
//! exactly one catalog name — mirroring the ACP envelope round-trip
//! convention in `crates/spur-acp/tests/executor_events_roundtrip.rs`.

use spur_tui::action::{Action, IssueAction, ViewId};
use spur_tui::commands::spur_local::{local_dispatch, SpurLocalSource};
use spur_tui::commands::Dispatch;

/// Reverse trace: given an action produced by `local_dispatch`, the single
/// catalog name that produced it. `None` means the action does not belong
/// to the local catalog at all (the dispatch table leaked a foreign action).
fn catalog_name_of(action: &Action) -> Option<&'static str> {
    match action {
        Action::ShowHelp => Some("help"),
        Action::ClearSession => Some("clear"),
        Action::RequestSessions => Some("sessions"),
        Action::BrainCommand { .. } => Some("brain"),
        Action::ListBrains => Some("brains"),
        Action::ShowSessionCost => Some("cost"),
        Action::Quit => Some("quit"),
        Action::ToggleVimMode => Some("vim"),
        Action::RefreshIssues => Some("issues"),
        Action::ThemeCommand { .. } => Some("theme"),
        Action::NotebookCommand { .. } => Some("notebook"),
        Action::NavigateTo(ViewId::AgentConfigBrowser { .. }) => Some("configure"),
        Action::NavigateTo(ViewId::PlanBrowser) => Some("sprints"),
        Action::NavigateTo(ViewId::ExploreBrowser) => Some("explore"),
        Action::Issue(IssueAction::WorkOn { .. }) => Some("work"),
        Action::Issue(IssueAction::ViewDetail { .. }) => Some("issue"),
        _ => None,
    }
}

#[test]
fn every_spur_local_entry_resolves_via_local_dispatch() {
    for entry in SpurLocalSource::entries() {
        assert!(
            local_dispatch(&entry.name, None).is_some(),
            "catalog entry /{} must resolve bare",
            entry.name
        );
        assert!(
            local_dispatch(&entry.name, Some("ignored")).is_some(),
            "catalog entry /{} must resolve with an arg",
            entry.name
        );
        // The neutral layer carries the entry's own name in its dispatch.
        assert!(
            matches!(&entry.dispatch, Dispatch::Local { name } if name == &entry.name),
            "entry /{} must carry its own name in Dispatch::Local",
            entry.name
        );
    }
}

#[test]
fn local_dispatch_round_trips_name_action_name() {
    // (name, arg) pairs that must resolve today. Router-intercepted names
    // (work/issue/theme/brain/notebook/configure) carry their arg into the
    // action exactly like the submit-router interceptions did.
    let cases: &[(&str, Option<&str>)] = &[
        ("help", None),
        ("help", Some("ignored")),
        ("clear", None),
        ("clear", Some("ignored")),
        ("sessions", None),
        ("brain", None),
        ("brain", Some("opencode")),
        ("brains", None),
        ("cost", None),
        ("quit", None),
        ("vim", None),
        ("vim", Some("ignored")),
        ("issues", None),
        ("theme", None),
        ("theme", Some("reload")),
        ("notebook", None),
        ("notebook", Some("new")),
        ("configure", None),
        ("configure", Some("codex")),
        ("sprints", None),
        ("explore", None),
        ("work", Some("bd-1")),
        ("issue", Some("show bd-1")),
    ];
    for (name, arg) in cases {
        let action =
            local_dispatch(name, *arg).unwrap_or_else(|| panic!("/{name} ({arg:?}) must resolve"));
        assert_eq!(
            catalog_name_of(&action),
            Some(*name),
            "reverse trace of /{name} ({arg:?})"
        );
    }

    // The catalog is closed: unknown names never resolve.
    assert!(local_dispatch("definitely-not-a-command", None).is_none());
    // Arg-requiring names stay unresolved without a usable arg.
    assert!(local_dispatch("work", None).is_none());
    assert!(local_dispatch("issue", None).is_none());
    assert!(local_dispatch("issue", Some("show   ")).is_none());
    assert!(local_dispatch("issue", Some("close bd-1")).is_none());
}

#[test]
fn local_dispatch_reproduces_router_built_actions() {
    // Spot-checks of the actions the submit router built inline before the
    // neutral dispatch existed — behavior must be reproduced verbatim.
    match local_dispatch("clear", None).expect("clear") {
        Action::ClearSession => {}
        other => panic!("expected ClearSession, got {other:?}"),
    }
    match local_dispatch("theme", Some("dark")).expect("theme") {
        Action::ThemeCommand { arg } => assert_eq!(arg, "dark"),
        other => panic!("expected ThemeCommand, got {other:?}"),
    }
    match local_dispatch("brain", None).expect("brain bare") {
        Action::BrainCommand { arg } => assert!(arg.is_empty()),
        other => panic!("expected BrainCommand, got {other:?}"),
    }
    match local_dispatch("configure", Some("codex")).expect("configure") {
        Action::NavigateTo(ViewId::AgentConfigBrowser { preselect }) => {
            assert_eq!(preselect.as_deref(), Some("codex"));
        }
        other => panic!("expected NavigateTo(AgentConfigBrowser), got {other:?}"),
    }
    match local_dispatch("work", Some("bd-1")).expect("work") {
        Action::Issue(IssueAction::WorkOn { id }) => assert_eq!(id, "bd-1"),
        other => panic!("expected Issue(WorkOn), got {other:?}"),
    }
    match local_dispatch("issue", Some("show bd-1")).expect("issue show") {
        Action::Issue(IssueAction::ViewDetail { id }) => assert_eq!(id, "bd-1"),
        other => panic!("expected Issue(ViewDetail), got {other:?}"),
    }
}

#[test]
fn spur_local_layer_lists_catalog_and_exclusive_names() {
    let layer = SpurLocalSource::layer();
    let entry_names: Vec<&str> = layer.entries.iter().map(|e| e.name.as_str()).collect();
    for entry in SpurLocalSource::entries() {
        assert!(
            entry_names.contains(&entry.name.as_str()),
            "layer must list /{}",
            entry.name
        );
    }
    let exclusive: Vec<&str> = layer.exclusive_names.iter().map(String::as_str).collect();
    for name in SpurLocalSource::exclusive_names() {
        assert!(exclusive.contains(name), "layer must keep {name} exclusive");
    }
}
