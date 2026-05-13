use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_acp::SessionId;
use spur_graph::validation::compute_anchor_hash;
use spur_graph::{artifact_from_facts, build_facts, write_artifact};
use spur_tui::commands::submit_router::assemble_blocks_with_code_mentions;
use spur_tui::components::input_bar::InputBar;
use spur_tui::mentions::{
    CodeMentionKind, CodeMentionValidationSpec, CompletionScope, IssueMentionDescriptor,
    MentionKind, MentionRegistry, WorkerMentionDescriptor, CODE_GRAPH_INDEX_ENV,
};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn file_mentions_index_and_fuzzy_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/foo.rs"), "// foo").unwrap();
    std::fs::write(root.join("src/bar.rs"), "// bar").unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

    let mut reg = MentionRegistry::new();
    let sid = SessionId::new();
    let hits = reg.query(CompletionScope::Session(&sid), root, "foo", 10);
    assert!(
        hits.iter().any(|h| h.display.contains("foo.rs")),
        "{:?}",
        hits
    );

    let all = reg.query(CompletionScope::Session(&sid), root, "", 10);
    assert!(!all.is_empty());
}

#[test]
fn brain_session_includes_workers_in_empty_query() {
    let mut reg = MentionRegistry::for_brain_session(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        description: Some("Refactors Rust".into()),
        tier: Some("specialist".into()),
    }]);
    let sid = SessionId::new();
    // Use an empty temp dir so file source returns nothing and worker entries
    // are always within the limit.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let hits = reg.query(CompletionScope::Session(&sid), cwd, "", 10);
    assert!(
        hits.iter()
            .any(|h| h.kind == MentionKind::Worker && h.display == "worker:claude-code"),
        "expected worker:claude-code in hits, got {:?}",
        hits.iter().map(|h| &h.display).collect::<Vec<_>>()
    );
}

#[test]
fn direct_session_excludes_workers() {
    let mut reg = MentionRegistry::for_direct_session();
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "", 50);
    assert!(
        !hits.iter().any(|h| h.kind == MentionKind::Worker),
        "direct session should not surface worker entries"
    );
}

#[test]
fn code_graph_env_missing_records_unobtrusive_hint() {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = std::env::var_os(CODE_GRAPH_INDEX_ENV);
    std::env::remove_var(CODE_GRAPH_INDEX_ENV);

    let reg = MentionRegistry::for_direct_session().with_code_graph_from_env();

    if let Some(previous) = previous {
        std::env::set_var(CODE_GRAPH_INDEX_ENV, previous);
    }
    assert_eq!(
        reg.code_graph_hint(),
        Some("Run 'spur graph build' to enable code-graph mentions")
    );
}

#[test]
fn extracted_graph_index_resolves_symbol_payload_through_registry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub struct Engine;\n\npub fn run() -> Engine {\n    Engine\n}\n",
    )
    .unwrap();
    let artifact_path = dir.path().join(".spur/graph-index.json");
    let facts = build_facts(dir.path()).expect("extract fixture worktree").0;
    let artifact = artifact_from_facts(&facts, dir.path()).expect("build artifact");
    write_artifact(&artifact, &artifact_path).expect("write artifact");

    let mut reg = MentionRegistry::for_direct_session().with_code_graph(&artifact_path);
    let sid = SessionId::new();
    let hits = reg.query(CompletionScope::Session(&sid), dir.path(), "Engine", 10);
    let symbol_uri = hits
        .iter()
        .find(|hit| hit.kind == MentionKind::CodeSymbol && hit.display == "Engine")
        .map(|hit| hit.uri.clone())
        .expect("Engine symbol mention");

    let payload = reg
        .lookup_code_payload(&symbol_uri)
        .expect("symbol payload from registry");
    assert_eq!(payload.authoritative.display, "Engine");
    assert_eq!(payload.authoritative.file_path, "src/lib.rs");
    assert_eq!(
        payload.extraction_hints.symbol_kind.as_deref(),
        Some("struct")
    );
}

#[test]
fn empty_query_pins_workers_first() {
    use std::io::Write;
    let workers: Vec<WorkerMentionDescriptor> = (0..6)
        .map(|i| WorkerMentionDescriptor {
            name: format!("worker-{}", i),
            description: None,
            tier: None,
        })
        .collect();
    let mut reg = MentionRegistry::for_brain_session(workers);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    // Seed competing files. Their display strings are short ("a.rs", "b.rs",
    // ...) so under the OLD length-sorted ranking they'd push workers
    // (display "worker:worker-N") off the top of the result.
    for ch in ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'] {
        let mut f = std::fs::File::create(tmp.path().join(format!("{}.rs", ch))).unwrap();
        writeln!(f, "// stub").unwrap();
    }
    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "", 20);
    let worker_count = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(
        worker_count,
        6,
        "expected first 6 rows to be workers, got {:?}",
        hits.iter()
            .take(6)
            .map(|h| (&h.kind, &h.display))
            .collect::<Vec<_>>()
    );
}

#[test]
fn empty_query_caps_workers_at_pin_cap() {
    use std::io::Write;
    // 10 workers in snapshot; only 6 (WORKER_PIN_CAP) should appear.
    let workers: Vec<WorkerMentionDescriptor> = (0..10)
        .map(|i| WorkerMentionDescriptor {
            name: format!("w{:02}", i),
            description: None,
            tier: None,
        })
        .collect();
    let mut reg = MentionRegistry::for_brain_session(workers);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    // Need enough competing files to fill the remaining 14 slots in
    // a limit=20 query — and to make the cap visible.
    for i in 0..20 {
        let mut f = std::fs::File::create(tmp.path().join(format!("file_{:02}.rs", i))).unwrap();
        writeln!(f, "// stub").unwrap();
    }
    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "", 20);
    let head_workers = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(head_workers, 6, "first 6 rows should be workers");
    // Rows 7-20 should not be workers (the cap capped at 6).
    let tail_workers = hits
        .iter()
        .skip(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(tail_workers, 0, "no workers should appear after the cap");
    // And there should be at least one file present in the tail.
    assert!(
        hits.iter().skip(6).any(|h| h.kind != MentionKind::Worker),
        "expected files after the worker pin"
    );
}

#[test]
fn typed_query_boosts_worker_in_ambiguous_match() {
    let mut reg = MentionRegistry::for_brain_session(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        description: None,
        tier: None,
    }]);
    let sid = SessionId::new();
    // Use a real workspace dir so FileMentionSource has files to compete.
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(CompletionScope::Session(&sid), &cwd, "cla", 5);
    assert!(
        hits.first()
            .map(|h| h.kind == MentionKind::Worker)
            .unwrap_or(false),
        "expected worker:claude-code at row 0 for 'cla', got {:?}",
        hits.iter()
            .map(|h| (&h.kind, &h.display))
            .collect::<Vec<_>>()
    );
}

#[test]
fn ranking_exact_symbol_outranks_exact_file_basename() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "config", 10);
    let symbol_idx = hit_position(&hits, "graph://symbol/symbol-config-struct");
    let file_idx = hit_position(&hits, "graph://file/file-config");

    assert!(
        symbol_idx < file_idx,
        "expected exact symbol Config before exact basename config.rs, got {:?}",
        hit_debug(&hits)
    );
}

#[test]
fn ranking_exact_file_basename_outranks_fuzzy_symbol() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "engine", 10);
    let file_idx = hit_position(&hits, "graph://file/file-engine");
    let symbol_idx = hit_position(&hits, "graph://symbol/symbol-engine-struct");

    assert!(
        file_idx < symbol_idx,
        "expected exact basename engine.rs before fuzzy symbol GraphEngine, got {:?}",
        hit_debug(&hits)
    );
}

#[test]
fn ranking_fuzzy_symbol_outranks_fuzzy_path() {
    let tmp = tempfile::tempdir().unwrap();
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": "ranking-fixture" },
            "files": [
                {
                    "stable_file_id": "file-graph-engine",
                    "file_path": "crates/example/src/graph_engine.rs"
                }
            ],
            "symbols": [
                {
                    "stable_symbol_id": "symbol-graph-engine",
                    "file_path": "crates/example/src/lib.rs",
                    "byte_range": [0, 20],
                    "line_range": [1, 3],
                    "entity_name": "GraphEngine",
                    "symbol_kind": "struct",
                    "anchor_hash": "anchor-graph-engine",
                    "enclosing_scope": "module lib"
                }
            ]
        }),
    );
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "GraEng", 10);
    let symbol_idx = hit_position(&hits, "graph://symbol/symbol-graph-engine");
    let file_idx = hit_position(&hits, "graph://file/file-graph-engine");

    assert!(
        symbol_idx < file_idx,
        "expected fuzzy symbol GraphEngine before fuzzy path graph_engine.rs, got {:?}",
        hit_debug(&hits)
    );
}

#[test]
fn ranking_collision_co_shows_symbol_and_file_unambiguously() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "Co", 10);
    let symbol_idx = hit_position(&hits, "graph://symbol/symbol-config-struct");
    let file_idx = hit_position(&hits, "graph://file/file-config");

    assert!(
        symbol_idx < file_idx,
        "expected Config before config.rs for collision query, got {:?}",
        hit_debug(&hits)
    );
    let file = hits
        .iter()
        .find(|hit| hit.uri == "graph://file/file-config")
        .expect("config.rs file row");
    assert_eq!(file.display, "crates/example/src/config.rs");
    assert_eq!(file.tag.as_deref(), Some("file"));
    let symbol = hits
        .iter()
        .find(|hit| hit.uri == "graph://symbol/symbol-config-struct")
        .expect("Config symbol row");
    assert_eq!(symbol.display, "Config");
    assert_eq!(symbol.tag.as_deref(), Some("symbol:struct"));
}

#[test]
fn empty_query_bounds_code_graph_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let files: Vec<_> = (0..4)
        .map(|idx| {
            serde_json::json!({
                "stable_file_id": format!("file-{idx:02}"),
                "file_path": format!("crates/example/src/file_{idx:02}.rs")
            })
        })
        .collect();
    let symbols: Vec<_> = (0..30)
        .map(|idx| {
            serde_json::json!({
                "stable_symbol_id": format!("symbol-{idx:02}"),
                "file_path": "crates/example/src/lib.rs",
                "byte_range": [idx, idx + 1],
                "line_range": [idx + 1, idx + 1],
                "entity_name": format!("Symbol{idx:02}"),
                "symbol_kind": "fn",
                "anchor_hash": format!("anchor-{idx:02}"),
                "enclosing_scope": "module lib"
            })
        })
        .collect();
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": "large-empty-fixture" },
            "files": files,
            "symbols": symbols
        }),
    );
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "", 20);
    let code_rows = hits
        .iter()
        .filter(|hit| matches!(hit.kind, MentionKind::CodeFile | MentionKind::CodeSymbol))
        .count();

    assert!(
        code_rows <= 8,
        "empty @ should not flood with code rows, got {code_rows}: {:?}",
        hit_debug(&hits)
    );
}

#[test]
fn worker_and_issue_rows_remain_visible_for_matching_typed_queries() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_brain_session(vec![WorkerMentionDescriptor {
        name: "codex".into(),
        description: Some("Writes patches".into()),
        tier: Some("generalist".into()),
    }])
    .with_code_graph(graph_path);
    reg.set_issue_snapshot(vec![issue("bd-7", "Coordinate codex work", Some("alice"))]);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "codex", 20);

    assert!(
        hits.iter()
            .any(|hit| hit.kind == MentionKind::Worker && hit.display == "worker:codex"),
        "expected matching worker row, got {:?}",
        hit_debug(&hits)
    );
    assert!(
        hits.iter()
            .any(|hit| hit.kind == MentionKind::Issue && hit.uri == "issue://beads/bd-7"),
        "expected matching issue row, got {:?}",
        hit_debug(&hits)
    );
}

#[test]
fn code_graph_registry_loads_fixture_files_and_symbols() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "", 50);

    assert_eq!(
        hits.iter()
            .filter(|hit| hit.kind == MentionKind::CodeFile)
            .count(),
        2
    );
    assert_eq!(
        hits.iter()
            .filter(|hit| hit.kind == MentionKind::CodeSymbol)
            .count(),
        4
    );
    assert!(hits.iter().any(|hit| {
        hit.kind == MentionKind::CodeFile
            && hit.uri == "graph://file/file-config"
            && hit.display == "crates/example/src/config.rs"
    }));

    let symbol = hits
        .iter()
        .find(|hit| hit.uri == "graph://symbol/symbol-engine-run-method")
        .expect("run symbol row");
    assert_eq!(symbol.display, "run");
    assert_eq!(
        symbol.secondary.as_deref(),
        Some("fn crates/example/src/engine.rs:12-20 impl GraphEngine")
    );
}

#[test]
fn code_graph_accept_and_submit_expands_fixture_symbol_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let graph_path = valid_config_fixture_copy(tmp.path());
    let source = config_fixture_source();
    let source_path = tmp.path().join("crates/example/src/config.rs");
    std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    std::fs::write(&source_path, &source).unwrap();

    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let symbol = reg
        .query(CompletionScope::Session(&sid), tmp.path(), "Config", 10)
        .into_iter()
        .find(|hit| hit.uri == "graph://symbol/symbol-config-struct")
        .expect("Config symbol row");

    let mut bar = InputBar::new();
    let atom_text = symbol
        .atom_text
        .clone()
        .unwrap_or_else(|| format!("@{}", symbol.display));
    bar.insert_atom(atom_text, symbol.uri.clone(), symbol.display);

    let blocks = assemble_blocks_with_code_mentions(
        &bar.text(),
        bar.protected_ranges(),
        &[],
        tmp.path(),
        |uri| reg.lookup_code_payload(uri),
    );
    let prompt = blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.as_str(),
            other => panic!("expected text-only code expansion, got {other:?}"),
        })
        .collect::<String>();

    assert!(prompt.contains("MENTION Config"), "{prompt}");
    assert!(prompt.contains("context_header:"), "{prompt}");
    assert!(!prompt.contains("source:\n"), "{prompt}");
    assert!(prompt.contains("topology_available_via_mcp:"), "{prompt}");
    assert!(
        prompt.contains("get_callers(\"graph://symbol/symbol-config-struct\")"),
        "{prompt}"
    );
}

#[test]
fn code_graph_lookup_payload_roundtrips_full_symbol_payload() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let _ = reg.query(
        CompletionScope::Session(&sid),
        tmp.path(),
        "GraphEngine",
        10,
    );

    let payload = reg
        .lookup_code_payload("graph://symbol/symbol-engine-struct")
        .expect("symbol payload");

    assert_eq!(payload.authoritative.display, "GraphEngine");
    assert_eq!(payload.authoritative.kind, CodeMentionKind::Symbol);
    assert_eq!(
        payload.authoritative.file_path,
        "crates/example/src/engine.rs"
    );
    assert_eq!(
        payload.extraction_hints.symbol_kind.as_deref(),
        Some("struct")
    );
    assert_eq!(
        payload.display_meta.enclosing_scope.as_deref(),
        Some("module engine")
    );
    assert_eq!(
        payload.display_meta.graph_index_version,
        "fixture-2026-05-11"
    );
    assert!(matches!(
        payload.authoritative.validation,
        CodeMentionValidationSpec::SymbolRange { .. }
    ));
}

#[test]
fn accepted_code_atom_carries_only_minimum_range_metadata() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let symbol = reg
        .query(
            CompletionScope::Session(&sid),
            tmp.path(),
            "GraphEngine",
            10,
        )
        .into_iter()
        .find(|hit| hit.uri == "graph://symbol/symbol-engine-struct")
        .expect("symbol row");

    let mut bar = InputBar::new();
    let atom_text = symbol
        .atom_text
        .clone()
        .unwrap_or_else(|| format!("@{}", symbol.display));
    bar.insert_atom(atom_text, symbol.uri.clone(), symbol.display.clone());
    let serialized = serde_json::to_value(bar.protected_ranges()).expect("serialize ranges");

    assert_eq!(bar.text(), "@GraphEngine");
    assert_eq!(bar.protected_ranges().len(), 1);
    assert_eq!(bar.protected_ranges()[0].uri, symbol.uri);
    assert_eq!(bar.protected_ranges()[0].name, "GraphEngine");
    assert_eq!(
        serialized,
        serde_json::json!([{
            "start": 0,
            "end": 12,
            "uri": "graph://symbol/symbol-engine-struct",
            "name": "GraphEngine"
        }])
    );
}

#[test]
fn pruning_code_payloads_after_atom_delete_removes_orphans() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let symbol = reg
        .query(
            CompletionScope::Session(&sid),
            tmp.path(),
            "GraphEngine",
            10,
        )
        .into_iter()
        .find(|hit| hit.uri == "graph://symbol/symbol-engine-struct")
        .expect("symbol row");

    let mut bar = InputBar::new();
    bar.insert_atom("@GraphEngine", symbol.uri.clone(), symbol.display);
    assert!(reg.lookup_code_payload(&symbol.uri).is_some());

    let _ = bar.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    reg.retain_code_payloads_for_uris(
        bar.protected_ranges()
            .iter()
            .map(|range| range.uri.as_str()),
    );

    assert!(bar.protected_ranges().is_empty());
    assert!(reg.lookup_code_payload(&symbol.uri).is_none());
}

#[test]
fn cache_rebuild_payload_loss_and_submit_prune_are_observable() {
    let graph_path = graph_fixture_path();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "Graph", 10);
    let kept = hits
        .iter()
        .find(|hit| hit.uri == "graph://symbol/symbol-engine-struct")
        .expect("kept symbol row");
    let orphan_uri = "graph://symbol/symbol-engine-run-method";

    let mut bar = InputBar::new();
    bar.insert_atom(
        kept.atom_text
            .clone()
            .unwrap_or_else(|| format!("@{}", kept.display)),
        kept.uri.clone(),
        kept.display.clone(),
    );
    assert!(reg.lookup_code_payload(&kept.uri).is_some());
    assert!(reg.lookup_code_payload(orphan_uri).is_some());

    reg.clear_cache();
    assert!(reg.lookup_code_payload(&kept.uri).is_none());

    let _ = reg.query(CompletionScope::Session(&sid), tmp.path(), "Graph", 10);
    assert!(reg.lookup_code_payload(&kept.uri).is_some());
    assert!(reg.lookup_code_payload(orphan_uri).is_some());

    reg.retain_code_payloads_for_uris(
        bar.protected_ranges()
            .iter()
            .map(|range| range.uri.as_str()),
    );

    assert!(reg.lookup_code_payload(&kept.uri).is_some());
    assert!(reg.lookup_code_payload(orphan_uri).is_none());
}

#[test]
fn missing_code_graph_artifact_leaves_existing_sources_available() {
    let missing = std::path::PathBuf::from("does/not/exist/graph.json");
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(missing);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("foo.rs"), "// foo").unwrap();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "foo", 10);

    assert!(hits.iter().any(|hit| hit.kind == MentionKind::File));
    assert!(!hits
        .iter()
        .any(|hit| matches!(hit.kind, MentionKind::CodeFile | MentionKind::CodeSymbol)));
}

#[test]
fn malformed_code_graph_artifact_leaves_existing_sources_available() {
    let tmp = tempfile::tempdir().unwrap();
    let graph_path = tmp.path().join("truncated.json");
    std::fs::write(&graph_path, r#"{"header":{"graph_index_version":"bad"},"#).unwrap();
    std::fs::write(tmp.path().join("foo.rs"), "// foo").unwrap();
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "foo", 10);

    assert!(hits.iter().any(|hit| hit.kind == MentionKind::File));
    assert!(!hits
        .iter()
        .any(|hit| matches!(hit.kind, MentionKind::CodeFile | MentionKind::CodeSymbol)));
}

#[test]
fn duplicate_symbol_ids_keep_first_row_and_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": "duplicate-fixture" },
            "files": [],
            "symbols": [
                {
                    "stable_symbol_id": "symbol-dup",
                    "file_path": "src/first.rs",
                    "byte_range": [0, 10],
                    "line_range": [1, 2],
                    "entity_name": "First",
                    "symbol_kind": "struct",
                    "anchor_hash": "1",
                    "enclosing_scope": "module first"
                },
                {
                    "stable_symbol_id": "symbol-dup",
                    "file_path": "src/second.rs",
                    "byte_range": [20, 30],
                    "line_range": [4, 5],
                    "entity_name": "Second",
                    "symbol_kind": "struct",
                    "anchor_hash": "2",
                    "enclosing_scope": "module second"
                }
            ]
        }),
    );
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "First", 10);
    let duplicate_rows: Vec<_> = hits
        .iter()
        .filter(|hit| hit.uri == "graph://symbol/symbol-dup")
        .collect();

    assert_eq!(duplicate_rows.len(), 1, "{:?}", hit_debug(&hits));
    assert_eq!(duplicate_rows[0].display, "First");
    let payload = reg
        .lookup_code_payload("graph://symbol/symbol-dup")
        .expect("deduplicated payload");
    assert_eq!(payload.authoritative.file_path, "src/first.rs");
}

#[test]
fn reversed_byte_range_artifact_disables_code_graph_rows() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("foo.rs"), "// foo").unwrap();
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": "reversed-fixture" },
            "files": [],
            "symbols": [
                {
                    "stable_symbol_id": "symbol-broken",
                    "file_path": "src/broken.rs",
                    "byte_range": [10, 9],
                    "line_range": [1, 2],
                    "entity_name": "Broken",
                    "symbol_kind": "fn",
                    "anchor_hash": "1",
                    "enclosing_scope": null
                }
            ]
        }),
    );
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let sid = SessionId::new();

    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "foo", 10);

    assert!(hits.iter().any(|hit| hit.kind == MentionKind::File));
    assert!(!hits
        .iter()
        .any(|hit| matches!(hit.kind, MentionKind::CodeFile | MentionKind::CodeSymbol)));
}

fn graph_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graph_index/sample.json")
}

fn issue(id: &str, title: &str, assignee: Option<&str>) -> IssueMentionDescriptor {
    IssueMentionDescriptor {
        id: id.to_string(),
        title: title.to_string(),
        source: spur_pm::PmSource::Beads,
        status: "open".to_string(),
        assignee: assignee.map(str::to_string),
        priority: None,
        issue_type: Some("task".to_string()),
        labels: vec!["mentions".to_string()],
        url: format!("https://example.test/{id}"),
        description: None,
    }
}

fn hit_position(hits: &[spur_tui::mentions::MentionEntry], uri: &str) -> usize {
    hits.iter()
        .position(|hit| hit.uri == uri)
        .unwrap_or_else(|| panic!("missing {uri}; hits: {:?}", hit_debug(hits)))
}

fn hit_debug(hits: &[spur_tui::mentions::MentionEntry]) -> Vec<(&MentionKind, &str, &str)> {
    hits.iter()
        .map(|hit| (&hit.kind, hit.display.as_str(), hit.uri.as_str()))
        .collect()
}

fn write_graph_fixture(root: &std::path::Path, value: serde_json::Value) -> std::path::PathBuf {
    let path = root.join("graph.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&value).expect("serialize graph fixture"),
    )
    .expect("write graph fixture");
    path
}

fn valid_config_fixture_copy(root: &std::path::Path) -> std::path::PathBuf {
    let fixture = std::fs::read_to_string(graph_fixture_path()).expect("read graph fixture");
    let mut value: serde_json::Value = serde_json::from_str(&fixture).expect("parse graph fixture");
    let source = config_fixture_source();
    let slice = &source[10..80];
    let hash = compute_anchor_hash(slice).to_string();
    let symbols = value
        .get_mut("symbols")
        .and_then(serde_json::Value::as_array_mut)
        .expect("fixture symbols");
    let config = symbols
        .iter_mut()
        .find(|symbol| {
            symbol
                .get("stable_symbol_id")
                .and_then(serde_json::Value::as_str)
                == Some("symbol-config-struct")
        })
        .expect("Config symbol");
    config["anchor_hash"] = serde_json::Value::String(hash);

    write_graph_fixture(root, value)
}

fn config_fixture_source() -> String {
    let source = concat!(
        "use a::b;\n",
        "pub struct Config {\n",
        "    pub path: String,\n",
        "}\n",
        "// fixture padding.\n",
        "pub fn after() {}\n"
    )
    .to_string();
    assert_eq!(source.find("pub struct Config"), Some(10));
    assert_eq!(
        &source[10..80],
        "pub struct Config {\n    pub path: String,\n}\n// fixture padding.\npub fn"
    );
    source
}
