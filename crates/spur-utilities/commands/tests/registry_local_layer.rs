//! Registry merge policy against a fixture frontend `LocalLayer` — the
//! neutral form of the TUI spur-local shadowing tests. Spec 2026-09-23
//! §3.2/§5 C2: a registry built with an injected layer must apply
//! exclusive-name shadowing to same-named dynamic and advertised entries,
//! with no TUI catalog in scope.

use spur_commands::entry::{CommandEntry, CommandSource, Dispatch};
use spur_commands::registry::{CommandRegistry, LocalLayer};

/// Fixture layer mirroring a frontend's meta-command catalog shape: one
/// exclusive meta-command (`/clear`) carrying `Dispatch::Local { name }`
/// pointing back at its own entry name.
fn fixture_local_layer() -> LocalLayer {
    LocalLayer {
        entries: vec![CommandEntry {
            name: "clear".into(),
            description: "fixture local clear".into(),
            hint: None,
            source: CommandSource::Spur,
            dispatch: Dispatch::Local {
                name: "clear".into(),
            },
            arg_picker_spec: None,
        }],
        exclusive_names: vec!["clear".into()],
    }
}

/// An agent-advertised dynamic entry competing with the local `/clear`.
fn agent_clear() -> CommandEntry {
    CommandEntry {
        name: "clear".into(),
        description: "agent's own clear".into(),
        hint: None,
        source: CommandSource::Agent {
            handle: "kiro".into(),
        },
        dispatch: Dispatch::PromptText {
            normalized: "/clear".into(),
        },
        arg_picker_spec: None,
    }
}

/// A spur-synthesized advertised entry competing with the local `/clear`.
fn advertised_clear() -> CommandEntry {
    CommandEntry {
        name: "clear".into(),
        description: "agent's clear".into(),
        hint: None,
        source: CommandSource::Advertised {
            handle: "codex".into(),
        },
        dispatch: Dispatch::SetSessionConfigOption {
            config_id: "clear".into(),
        },
        arg_picker_spec: None,
    }
}

#[test]
fn exclusive_local_name_shadows_agent_entry() {
    let mut registry = CommandRegistry::new(fixture_local_layer());
    registry.set_agent_commands("kiro", vec![agent_clear()]);

    let list = registry.list();
    let clear_entries: Vec<_> = list.iter().filter(|e| e.name == "clear").collect();
    assert_eq!(
        clear_entries.len(),
        1,
        "agent /clear must be shadowed by the exclusive local /clear"
    );
    assert!(
        matches!(clear_entries[0].source, CommandSource::Spur),
        "the surviving /clear must be the local entry"
    );
    assert!(
        matches!(&clear_entries[0].dispatch, Dispatch::Local { name } if name == "clear"),
        "the surviving /clear must keep its neutral local dispatch"
    );
}

#[test]
fn exclusive_local_name_shadows_advertised_entry() {
    let mut registry = CommandRegistry::new(fixture_local_layer());
    registry.set_advertised_commands("codex", vec![advertised_clear()]);

    let clear_entries: Vec<_> = registry
        .list()
        .into_iter()
        .filter(|e| e.name == "clear")
        .collect();
    assert_eq!(
        clear_entries.len(),
        1,
        "advertised /clear must be shadowed by the exclusive local /clear"
    );
    assert!(
        matches!(clear_entries[0].source, CommandSource::Spur),
        "the surviving /clear must be the local entry"
    );
}

#[test]
fn empty_layer_builds_a_bare_registry() {
    let registry = CommandRegistry::new(LocalLayer::empty());
    assert!(
        registry.list().is_empty(),
        "an empty layer must not inject any meta-commands"
    );
}
