//! sud-m0 characterization: pins today's `@`-mention registry behavior
//! through public paths only (`spur_tui::mentions::...`), so the
//! spur-utilities decoupling (spec §5 Phase M) must keep every assertion
//! green without editing this file. No `mentions::hint` usage (sud-m-hint
//! relocates it). Fixtures live in tempdirs; no network.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use spur_acp::{Column, DatasourceEntry, DatasourceKind, Table};
use spur_graph::{artifact_from_facts, build_facts, write_artifact_parquet, WriteOptions};
use spur_tui::mentions::{
    CompletionScope, IssueMentionDescriptor, MentionEntry, MentionKind, MentionRegistry,
    WorkerMentionDescriptor,
};

/// Serializes tests that mutate process-global env vars within this binary.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct CatalogPathGuard {
    original: Option<std::ffi::OsString>,
}

impl CatalogPathGuard {
    fn set(path: &Path) -> Self {
        let original = std::env::var_os(spur_acp::agent_model_catalog::CACHE_PATH_ENV);
        std::env::set_var(spur_acp::agent_model_catalog::CACHE_PATH_ENV, path);
        Self { original }
    }
}

impl Drop for CatalogPathGuard {
    fn drop(&mut self) {
        if let Some(path) = self.original.take() {
            std::env::set_var(spur_acp::agent_model_catalog::CACHE_PATH_ENV, path);
        } else {
            std::env::remove_var(spur_acp::agent_model_catalog::CACHE_PATH_ENV);
        }
    }
}

fn worker(name: &str, cli_identity: &str) -> WorkerMentionDescriptor {
    WorkerMentionDescriptor {
        name: name.into(),
        kind: spur_acp::AgentKind::CodexAcp,
        cli_identity: cli_identity.into(),
        description: Some("characterization worker".into()),
        tier: Some("specialist".into()),
    }
}

fn issue(id: &str, title: &str, assignee: Option<&str>) -> IssueMentionDescriptor {
    IssueMentionDescriptor {
        id: id.to_string(),
        title: title.to_string(),
        source: spur_pm::PmSource::Beads,
        status: "in_progress".to_string(),
        assignee: assignee.map(str::to_string),
        priority: Some(2),
        issue_type: Some("task".to_string()),
        labels: vec!["ux".to_string()],
        url: format!("https://example.test/{id}"),
        description: None,
    }
}

fn datasource(name: &str, group: Option<&str>) -> DatasourceEntry {
    DatasourceEntry {
        name: name.into(),
        path: format!("./data/{name}.csv"),
        kind: DatasourceKind::Csv,
        group: group.map(str::to_string),
        columns: vec![Column {
            name: "region".into(),
            sql_type: "VARCHAR".into(),
        }],
        row_count: Some(10),
        tables: vec![Table {
            name: "line_items".into(),
            columns: vec![Column {
                name: "sku".into(),
                sql_type: "VARCHAR".into(),
            }],
            row_count: Some(20),
        }],
    }
}

// ---------------------------------------------------------------- fixtures

fn graph_fixture_json() -> serde_json::Value {
    let raw = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/graph_index/sample.json"),
    )
    .expect("read graph fixture json");
    serde_json::from_str(&raw).expect("parse graph fixture json")
}

/// Convert the human-editable JSON fixture into the parquet artifact the
/// loader accepts (same conversion the mention_registry.rs tests perform).
fn write_graph_fixture(root: &Path, value: serde_json::Value) -> PathBuf {
    let mut artifact: spur_graph::GraphIndexArtifact =
        serde_json::from_value(value).expect("graph fixture json must match artifact shape");
    let n_files = artifact.files.len();
    artifact.file_node_ids = (0..n_files as u64).map(spur_graph::NodeId).collect();
    artifact.symbol_node_ids = (0..artifact.symbols.len() as u64)
        .map(|i| spur_graph::NodeId(n_files as u64 + i))
        .collect();
    write_artifact_parquet(&artifact, root, WriteOptions::default(), Vec::new())
        .expect("write parquet graph fixture");
    root.to_path_buf()
}

/// Shared parquet copy of tests/fixtures/graph_index/sample.json, kept in a
/// dedicated tempdir so its shard files never enter a walked workspace.
fn sample_graph_path() -> PathBuf {
    static FIXTURE: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_dir, path) = FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("graph fixture tempdir");
        let path = write_graph_fixture(dir.path(), graph_fixture_json());
        (dir, path)
    });
    path.clone()
}

fn write_agent_profile(root: &Path, name: &str) {
    let dir = root.join(".spur/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!("---\nname: {name}\ndescription: {name} profile\n---\nbody\n"),
    )
    .unwrap();
}

fn write_catalog(
    root: &Path,
    worker_name: &str,
    cli_identity: &str,
    models: &[&str],
    efforts: &[&str],
) -> PathBuf {
    use spur_acp::agent_model_catalog::{
        write, AgentModelCatalogV1, ConfigOptionChoice, WorkerCatalogEntry,
    };
    let path = root.join("agent-model-catalog.json");
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        worker_name.to_string(),
        WorkerCatalogEntry {
            probed_at: chrono::Utc::now(),
            cli_identity: cli_identity.to_string(),
            models: models
                .iter()
                .map(|value| ConfigOptionChoice {
                    value: value.to_string(),
                    name: value.to_string(),
                    description: None,
                })
                .collect(),
            efforts: efforts
                .iter()
                .map(|value| ConfigOptionChoice {
                    value: value.to_string(),
                    name: value.to_string(),
                    description: None,
                })
                .collect(),
        },
    );
    write(
        &path,
        &AgentModelCatalogV1 {
            version: 1,
            entries,
        },
    )
    .unwrap();
    path
}

fn section_slice<'a>(hits: &'a [MentionEntry], header: &str) -> &'a [MentionEntry] {
    let start = hits
        .iter()
        .position(|hit| hit.section_header == Some(header))
        .unwrap_or_else(|| panic!("missing {header} section: {:?}", hit_debug(hits)))
        + 1;
    let end = hits[start..]
        .iter()
        .position(|hit| hit.section_header.is_some())
        .map(|offset| start + offset)
        .unwrap_or(hits.len());
    &hits[start..end]
}

fn hit_debug(hits: &[MentionEntry]) -> Vec<(&MentionKind, &str, &str)> {
    hits.iter()
        .map(|hit| (&hit.kind, hit.display.as_str(), hit.uri.as_str()))
        .collect()
}

// -------------------------------------------------- empty-query + ordering

#[test]
fn empty_query_sections_order_and_caps() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..8 {
        std::fs::write(tmp.path().join(format!("f{i}.rs")), "// stub").unwrap();
    }

    let mut reg = MentionRegistry::for_brain_session(
        (0..6)
            .map(|i| worker(&format!("w{i}"), "probe-cli"))
            .collect(),
    )
    .with_code_graph(sample_graph_path());
    reg.set_issue_snapshot(
        (1..=5)
            .map(|i| issue(&format!("bd-{i}"), &format!("T{i}"), Some("alice")))
            .collect(),
    );
    reg.set_datasource_snapshot(
        ["ds-a", "ds-b", "ds-c", "ds-d", "ds-e"]
            .iter()
            .map(|name| datasource(name, Some("quarterly")))
            .collect(),
    );

    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "", 64);

    let headers: Vec<_> = hits.iter().filter_map(|h| h.section_header).collect();
    assert_eq!(headers, vec!["Workers", "Files", "Issues", "Data", "Code"]);

    let workers = section_slice(&hits, "Workers");
    assert_eq!(
        workers
            .iter()
            .map(|h| h.display.as_str())
            .collect::<Vec<_>>(),
        vec!["worker:w0", "worker:w1", "worker:w2", "worker:w3"],
        "WORKER_PIN_CAP=4, length-then-lex order"
    );

    let files = section_slice(&hits, "Files");
    assert_eq!(
        files.iter().map(|h| h.display.as_str()).collect::<Vec<_>>(),
        vec!["f0.rs", "f1.rs", "f2.rs", "f3.rs", "f4.rs", "f5.rs"],
        "FILE_CAP=6, depth-then-length-then-lex order"
    );

    let issues = section_slice(&hits, "Issues");
    assert_eq!(
        issues
            .iter()
            .map(|h| h.display.as_str())
            .collect::<Vec<_>>(),
        vec!["bd-1 T1", "bd-2 T2", "bd-3 T3"],
        "ISSUE_CAP=3 preserves snapshot order"
    );

    let data = section_slice(&hits, "Data");
    assert_eq!(
        data.iter().map(|h| h.display.as_str()).collect::<Vec<_>>(),
        vec!["ds-a", "ds-b", "ds-c", "ds-d"],
        "DATASOURCE_CAP=4, display order"
    );

    let code = section_slice(&hits, "Code");
    let code_shapes: Vec<_> = code
        .iter()
        .map(|h| (&h.kind, h.display.as_str(), h.uri.as_str()))
        .collect();
    assert_eq!(
        code_shapes,
        vec![
            (
                &MentionKind::CodeFile,
                "crates/example/src/config.rs",
                "graph://file/file-config"
            ),
            (
                &MentionKind::CodeFile,
                "crates/example/src/engine.rs",
                "graph://file/file-engine"
            ),
            (
                &MentionKind::CodeSymbol,
                "run",
                "graph://symbol/symbol-engine-run-method"
            ),
        ],
        "CODE_CAP=3: files before symbols, backfill by shortest entity_name"
    );
    assert_eq!(
        code[2].atom_text.as_deref(),
        Some("@impl GraphEngine::run"),
        "scoped symbols qualify their atom text"
    );

    let header_rows: Vec<_> = hits
        .iter()
        .filter(|h| h.section_header.is_some())
        .map(|h| h.display.as_str())
        .collect();
    assert_eq!(
        header_rows,
        vec![
            "── Workers ──",
            "── Files ──",
            "── Issues ──",
            "── Data ──",
            "── Code ──"
        ]
    );
}

#[test]
fn empty_query_truncates_at_limit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f0.rs"), "// stub").unwrap();
    let mut reg =
        MentionRegistry::for_brain_session((0..6).map(|i| worker(&format!("w{i}"), "x")).collect());
    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "", 3);
    assert_eq!(hits.len(), 3, "rows truncate at the caller limit");
    assert_eq!(hits[0].section_header, Some("Workers"));
    assert_eq!(hits[1].display, "worker:w0");
    assert_eq!(hits[2].display, "worker:w1");
}

#[test]
fn typed_query_emits_no_section_headers() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f0.rs"), "// stub").unwrap();
    let mut reg = MentionRegistry::for_direct_session();
    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "f0", 10);
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| h.section_header.is_none()),
        "typed queries must not carry section headers: {:?}",
        hit_debug(&hits)
    );
    assert!(hits
        .iter()
        .any(|h| h.kind == MentionKind::File && h.display == "f0.rs"));
}

// ---------------------------------------------------------- file traversal

#[test]
fn file_traversal_skips_hidden_and_gitignored() {
    let tmp = tempfile::tempdir().unwrap();
    // Mark the tempdir as a git worktree so .gitignore rules activate.
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "ignored.rs\n").unwrap();
    std::fs::write(tmp.path().join("vis.rs"), "// visible").unwrap();
    std::fs::write(tmp.path().join("ignored.rs"), "// ignored").unwrap();
    std::fs::write(tmp.path().join(".hidden.rs"), "// hidden").unwrap();
    std::fs::create_dir_all(tmp.path().join(".hiddendir")).unwrap();
    std::fs::write(tmp.path().join(".hiddendir/x.rs"), "// hidden").unwrap();

    let mut reg = MentionRegistry::for_direct_session();
    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "", 64);
    let rows: Vec<_> = hits
        .iter()
        .filter(|h| h.section_header.is_none())
        .map(|h| h.display.as_str())
        .collect();

    assert!(rows.contains(&"vis.rs"), "visible file listed: {rows:?}");
    assert_eq!(
        rows,
        vec!["vis.rs"],
        "hidden and gitignored paths must be skipped"
    );
}

#[test]
fn directory_rows_carry_trailing_slash_display() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/lib.rs"), "// lib").unwrap();
    std::fs::write(tmp.path().join("a.rs"), "// a").unwrap();

    let mut reg = MentionRegistry::for_direct_session();
    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "", 64);

    let dir = hits
        .iter()
        .find(|h| h.kind == MentionKind::Directory)
        .expect("directory row present");
    assert_eq!(dir.display, "src/");
    assert_eq!(
        dir.uri,
        format!("file://{}", tmp.path().join("src").display())
    );
    assert!(
        hits.iter()
            .any(|h| h.kind == MentionKind::File && h.display == "src/lib.rs"),
        "nested file listed too: {:?}",
        hit_debug(&hits)
    );
}

#[test]
fn non_ascii_and_spaced_names_keep_raw_unencoded_uris() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("café.rs"), "// unicode").unwrap();
    std::fs::write(tmp.path().join("データ.rs"), "// unicode").unwrap();
    std::fs::write(tmp.path().join("with space.rs"), "// spaced").unwrap();

    let mut reg = MentionRegistry::for_direct_session();
    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "", 64);

    for name in ["café.rs", "データ.rs", "with space.rs"] {
        let hit = hits
            .iter()
            .find(|h| h.display == name)
            .unwrap_or_else(|| panic!("missing {name}: {:?}", hit_debug(&hits)));
        assert_eq!(hit.kind, MentionKind::File);
        assert_eq!(
            hit.uri,
            format!("file://{}", tmp.path().join(name).display())
        );
    }
    // File URIs are raw paths — no percent-encoding anywhere.
    assert!(
        hits.iter().all(|h| !h.uri.contains('%')),
        "{:?}",
        hit_debug(&hits)
    );

    let typed = reg.query(CompletionScope::PreSession, tmp.path(), "café", 10);
    assert!(
        typed.iter().any(|h| h.display == "café.rs"),
        "typed non-ASCII query matches: {:?}",
        hit_debug(&typed)
    );
}

// ------------------------------------------------------ composed workers

#[test]
fn typed_query_composes_full_worker_slot_cascade() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    write_agent_profile(tmp.path(), "spur-char-m0");
    let catalog_path = write_catalog(tmp.path(), "codex", "probe-cli", &["gpt-5.5"], &["high"]);
    let _catalog_guard = CatalogPathGuard::set(&catalog_path);

    let mut reg = MentionRegistry::for_brain_session(vec![worker("codex", "probe-cli")]);
    let hits = reg.query(
        CompletionScope::PreSession,
        tmp.path(),
        "codex,spur-char-m0,gpt-5.5,high",
        10,
    );

    let worker_rows: Vec<&MentionEntry> = hits
        .iter()
        .filter(|h| h.kind == MentionKind::Worker)
        .collect();
    assert_eq!(
        worker_rows.len(),
        1,
        "composed row replaces the base: {:?}",
        hit_debug(&hits)
    );
    let composed = worker_rows[0];
    assert_eq!(
        composed.uri,
        "worker://codex?agent=spur-char-m0&model=gpt-5.5&effort=high"
    );
    assert_eq!(
        composed.atom_text.as_deref(),
        Some("@worker:codex agent=spur-char-m0 model=gpt-5.5 effort=high")
    );
    assert_eq!(
        composed.display,
        "worker:codex agent=spur-char-m0 model=gpt-5.5 effort=high"
    );
    assert_eq!(composed.agent.as_deref(), Some("spur-char-m0"));
    assert_eq!(composed.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(composed.effort.as_deref(), Some("high"));

    // Fresh catalog with matching CLI identity: no probe requests.
    assert!(
        reg.drain_agent_model_catalog_probe_requests().is_empty(),
        "fresh matching catalog must not request a probe"
    );
    assert_eq!(reg.agent_model_catalog_probe_hint(), None);
}

#[test]
fn missing_catalog_requests_model_probe_with_hint() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    // Point the catalog env at a path that does not exist.
    let _catalog_guard = CatalogPathGuard::set(&tmp.path().join("no-catalog.json"));

    let mut reg = MentionRegistry::for_brain_session(vec![worker("codex", "probe-cli")]);
    let _hits = reg.query(CompletionScope::PreSession, tmp.path(), "codex", 10);

    assert_eq!(
        reg.drain_agent_model_catalog_probe_requests(),
        vec!["codex".to_string()],
        "missing catalog entry must request a probe"
    );
    assert_eq!(
        reg.agent_model_catalog_probe_hint(),
        Some(format!("fetching codex models\u{2026}"))
    );
    reg.mark_agent_model_catalog_probe_completed("codex");
    assert_eq!(reg.agent_model_catalog_probe_hint(), None);
}

// ------------------------------------------------------------ issue rows

#[test]
fn issue_rows_carry_metadata_and_label_search() {
    let mut reg = MentionRegistry::for_direct_session();
    reg.set_issue_snapshot(vec![
        issue("bd-1", "Fix mention picker", Some("alice")),
        IssueMentionDescriptor {
            id: "gh-9".into(),
            title: "GitHub tracked".into(),
            source: spur_pm::PmSource::GitHub,
            status: "open".into(),
            assignee: None,
            priority: None,
            issue_type: None,
            labels: vec!["infra".into()],
            url: "https://example.test/gh-9".into(),
            description: None,
        },
    ]);

    let cwd = tempfile::tempdir().unwrap();
    let by_id = reg.query(CompletionScope::PreSession, cwd.path(), "bd-1", 10);
    assert_eq!(by_id.len(), 1, "{:?}", hit_debug(&by_id));
    let row = &by_id[0];
    assert_eq!(row.kind, MentionKind::Issue);
    assert_eq!(row.uri, "issue://beads/bd-1");
    assert_eq!(row.display, "bd-1 Fix mention picker");
    assert_eq!(row.secondary.as_deref(), Some("in_progress · alice"));
    assert_eq!(row.tag.as_deref(), Some("P2"));
    assert_eq!(row.atom_text.as_deref(), Some("@bd-1"));

    // search_text includes labels: "ux" matches only bd-1.
    let by_label = reg.query(CompletionScope::PreSession, cwd.path(), "ux", 10);
    assert_eq!(
        by_label.len(),
        1,
        "label search must surface bd-1 only: {:?}",
        hit_debug(&by_label)
    );
    assert_eq!(by_label[0].uri, "issue://beads/bd-1");

    let by_source = reg.query(CompletionScope::PreSession, cwd.path(), "gh-9", 10);
    let gh = by_source
        .iter()
        .find(|h| h.uri == "issue://github/gh-9")
        .expect("github slug row");
    assert_eq!(gh.secondary.as_deref(), Some("open"));
    assert_eq!(gh.tag, None);
}

// ------------------------------------------------------- datasource rows

#[test]
fn datasource_rows_carry_tag_secondary_and_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let mut reg = MentionRegistry::for_direct_session();
    reg.set_datasource_snapshot(vec![
        datasource("ds-a", Some("quarterly")),
        datasource("ds-b", None),
    ]);

    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "ds-b", 10);
    let row = hits
        .iter()
        .find(|h| h.kind == MentionKind::Datasource)
        .expect("typed datasource row");
    assert_eq!(row.uri, "datasource://ds-b");
    assert_eq!(row.display, "ds-b");
    assert_eq!(row.tag.as_deref(), Some("csv"));
    assert_eq!(row.secondary.as_deref(), Some("./data/ds-b.csv"));
    assert_eq!(row.atom_text.as_deref(), Some("@ds-b"));

    // Group label renders in the secondary line.
    let grouped = reg.query(CompletionScope::PreSession, tmp.path(), "ds-a", 10);
    let row = grouped
        .iter()
        .find(|h| h.kind == MentionKind::Datasource)
        .expect("grouped datasource row");
    assert_eq!(
        row.secondary.as_deref(),
        Some("quarterly - ./data/ds-a.csv")
    );

    // The prompt hint is cached with the row once the source is built.
    let hint = reg
        .lookup_datasource_hint("datasource://ds-b")
        .expect("datasource prompt hint");
    assert!(hint.contains("columns:"), "{hint}");
    assert!(hint.contains("region VARCHAR"), "{hint}");
}

// ------------------------------------------------------- code-graph rows

#[test]
fn code_graph_typed_query_pins_symbol_row_shape_and_hydration() {
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(sample_graph_path());
    let tmp = tempfile::tempdir().unwrap();

    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "load_config", 10);
    assert_eq!(hits.len(), 1, "{:?}", hit_debug(&hits));
    let row = &hits[0];
    assert_eq!(row.kind, MentionKind::CodeSymbol);
    assert_eq!(row.uri, "graph://symbol/symbol-load-fn");
    assert_eq!(row.display, "load_config");
    assert_eq!(
        row.secondary.as_deref(),
        Some("module config::load_config · crates/example/src/config.rs:9 (fn)")
    );
    assert_eq!(row.tag.as_deref(), Some("symbol:fn"));
    assert_eq!(
        row.atom_text.as_deref(),
        Some("@module config::load_config")
    );

    // Only returned symbol payloads are hydrated.
    assert!(reg
        .lookup_code_payload("graph://symbol/symbol-load-fn")
        .is_some());
    assert!(reg
        .lookup_code_payload("graph://symbol/symbol-engine-struct")
        .is_none());
}

#[test]
fn code_graph_typed_query_pins_file_row_shape() {
    let mut reg = MentionRegistry::for_direct_session().with_code_graph(sample_graph_path());
    let tmp = tempfile::tempdir().unwrap();

    // "engine.rs" matches the exact file row first, then the symbols whose
    // candidate *file path* fuzzy-matches the query (file-path fallback in
    // candidate ranking). Exact order is pinned.
    let hits = reg.query(CompletionScope::PreSession, tmp.path(), "engine.rs", 10);
    let shapes: Vec<_> = hits
        .iter()
        .map(|h| (&h.kind, h.display.as_str(), h.uri.as_str()))
        .collect();
    assert_eq!(
        shapes,
        vec![
            (
                &MentionKind::CodeFile,
                "crates/example/src/engine.rs",
                "graph://file/file-engine"
            ),
            (
                &MentionKind::CodeSymbol,
                "run",
                "graph://symbol/symbol-engine-run-method"
            ),
            (
                &MentionKind::CodeSymbol,
                "GraphEngine",
                "graph://symbol/symbol-engine-struct"
            ),
        ],
        "exact file row first, then file-path-matched symbols"
    );
    let row = &hits[0];
    assert_eq!(row.tag.as_deref(), Some("file"));
    assert_eq!(row.atom_text, None);
    assert_eq!(
        row.code_path.as_deref(),
        Some("crates/example/src/engine.rs")
    );
}

#[test]
fn code_graph_candidates_from_extracted_fixture_index() {
    // End-to-end fixture: build a real artifact from extracted facts, then
    // resolve a symbol row through the registry.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub struct Engine;\n\npub fn run() -> Engine {\n    Engine\n}\n",
    )
    .unwrap();
    let facts = build_facts(dir.path(), None)
        .expect("extract fixture worktree")
        .0;
    let artifact = artifact_from_facts(&facts, dir.path()).expect("build artifact");
    let artifact_path = write_artifact_parquet(
        &artifact,
        &dir.path().join(".spur/graph"),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");

    let mut reg = MentionRegistry::for_direct_session().with_code_graph(artifact_path);
    let hits = reg.query(CompletionScope::PreSession, dir.path(), "Engine", 10);
    let symbol = hits
        .iter()
        .find(|hit| hit.kind == MentionKind::CodeSymbol && hit.display == "Engine")
        .expect("Engine symbol row");
    assert_eq!(symbol.code_path.as_deref(), Some("src/lib.rs"));
    assert!(symbol.uri.starts_with("graph://symbol/"));
}
