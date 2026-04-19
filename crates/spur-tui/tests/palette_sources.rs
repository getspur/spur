use spur_tui::commands::registry::CommandRegistry;
use spur_tui::components::palette::{PaletteKind, PalettePayload};
use spur_tui::components::palette_sources::{CommandSource, PaletteSource};

#[test]
fn command_source_yields_all_registered_commands_as_command_kind() {
    let registry = CommandRegistry::default();
    let src = CommandSource::new(&registry);
    let results = src.collect();

    assert!(!results.is_empty(), "default registry should contain builtin commands");
    assert!(results.iter().all(|r| r.kind == PaletteKind::Command));

    // Every result has a command payload with a non-empty name.
    for r in &results {
        match &r.payload {
            PalettePayload::Command { name } => assert!(!name.is_empty()),
            _ => panic!("expected Command payload, got {:?}", r.payload),
        }
    }
}
