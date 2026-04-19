use spur_tui::commands::registry::CommandRegistry;
use spur_tui::components::palette::{PaletteKind, PalettePayload};
use spur_tui::components::palette_sources::{CommandSource, PaletteSource, SessionSource, WorkerSource};
use spur_tui::session_metadata::{SessionEntry, SessionMetadata};

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

#[test]
fn session_source_yields_session_kind_rows_with_title_as_label() {
    let mut meta = SessionMetadata::default();
    meta.sessions.insert(
        "sess-1".to_string(),
        SessionEntry {
            title_override: Some("refactor-auth".to_string()),
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "sess-2".to_string(),
        SessionEntry {
            title_override: None, // falls back to session_id as label
            ..Default::default()
        },
    );

    let src = SessionSource::from_metadata(&meta);
    let results = src.collect();
    assert_eq!(results.len(), 2);

    let labels: Vec<&str> = results.iter().map(|r| r.label.as_str()).collect();
    assert!(labels.contains(&"refactor-auth"));
    assert!(labels.contains(&"sess-2"));

    for r in &results {
        assert!(matches!(r.kind, spur_tui::components::palette::PaletteKind::Session));
    }
}

use spur_core::lineage::projection::ExecutorLineage;

#[test]
fn worker_source_yields_no_rows_for_empty_lineage() {
    let lineage = ExecutorLineage::new();
    let src = WorkerSource::from_lineage(&lineage);
    assert_eq!(src.collect().len(), 0);
}

use spur_tui::components::palette_sources::TraceSource;

#[test]
fn trace_source_handles_empty_trace() {
    let src = TraceSource::from_empty();
    assert_eq!(src.collect().len(), 0);
}
