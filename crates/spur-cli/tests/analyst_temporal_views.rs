use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use spur_cli::commands::analyst::{self, AnalystBuildOptions};
use spur_graph::store::lance_sections::{
    write_sections_dataset_best_effort_with_options, SectionEmbeddingOptions,
    CODE_SYMBOLS_DATASET_DIR, SECTIONS_DATASET_DIR,
};
use spur_graph::{
    ChangeKind, CommitArtifact, Confidence, EdgeEndpoint, GraphArtifactSidecarRowCounts,
    GraphArtifactSidecarStatus, GraphEdgeArtifact, GraphEdgeKind, GraphFileArtifact,
    GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader, GraphSymbolArtifact, NodeId,
    RelationKind, RenamePrev, SnapshotKey, SymbolSnapshotArtifact, TemporalEdgeArtifact,
    WriteOptions,
};

const INIT_SQL: &str = include_str!("../../spur-context/analyst/init.sql");

fn duckdb_cli_present() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("duckdb").is_file()))
        .unwrap_or(false)
}

fn query_csv(db_path: &Path, sql: &str) -> String {
    let output = Command::new("duckdb")
        .args(["-csv", "-noheader"])
        .arg(db_path)
        .args(["-c", sql])
        .output()
        .expect("duckdb query");
    assert!(
        output.status.success(),
        "duckdb query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("duckdb stdout utf8")
}

fn run_duckdb_sql(db_path: &Path, sql: &str) {
    let mut child = Command::new("duckdb")
        .arg(db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn duckdb");
    child
        .stdin
        .as_mut()
        .expect("duckdb stdin")
        .write_all(sql.as_bytes())
        .expect("write duckdb sql");

    let output = child.wait_with_output().expect("duckdb wait");
    assert!(
        output.status.success(),
        "duckdb init SQL failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn structural_init_sql_for_test(artifact_dir: &Path) -> String {
    let artifact_dir_sql = artifact_dir.display().to_string().replace('\'', "''");
    let sql = INIT_SQL
        .replace("__SPUR_GRAPH_ARTIFACT_DIR__", &artifact_dir_sql)
        .replace("__SPUR_LANCE_ATTACH_SQL__", "");

    let mut filtered = String::new();
    let mut skip_property_graph = false;
    for line in sql.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("INSTALL ") || trimmed.starts_with("LOAD ") {
            continue;
        }
        if trimmed.starts_with("CREATE OR REPLACE PROPERTY GRAPH code") {
            skip_property_graph = true;
            continue;
        }
        if skip_property_graph {
            if trimmed == ");" {
                skip_property_graph = false;
            }
            continue;
        }
        filtered.push_str(line);
        filtered.push('\n');
    }
    filtered
}

fn build_analyst_or_skip(root: &Path, artifact_dir: &Path, db_path: &Path) -> bool {
    analyst::build(
        root,
        AnalystBuildOptions {
            artifact_dir: Some(artifact_dir.to_path_buf()),
            db_path: Some(db_path.to_path_buf()),
            quiet: true,
        },
    )
    .expect("analyst build");

    if !db_path.is_file() {
        eprintln!("skipping: duckdb CLI present but analyst init SQL did not produce a DB");
        return false;
    }
    true
}

fn fake_duckdb_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn find_duckdb_on_path(path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join("duckdb"))
        .find(|candidate| candidate.is_file())
}

fn shell_single_quote(value: &str) -> String {
    value.replace('\'', "'\\''")
}

fn build_analyst_with_fake_duckdb(
    root: &Path,
    artifact_dir: &Path,
    db_path: &Path,
    probe_row_count: usize,
) -> String {
    build_analyst_with_fake_duckdb_probe_counts(
        root,
        artifact_dir,
        db_path,
        probe_row_count,
        probe_row_count,
    )
}

fn build_analyst_with_fake_duckdb_probe_counts(
    root: &Path,
    artifact_dir: &Path,
    db_path: &Path,
    section_probe_row_count: usize,
    code_symbol_probe_row_count: usize,
) -> String {
    let _guard = fake_duckdb_env_lock().lock().expect("fake duckdb env lock");
    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let real_duckdb = find_duckdb_on_path(&original_path)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let shim_dir = root.join("duckdb-shim");
    let capture_path = root.join("captured-analyst.sql");
    std::fs::create_dir_all(&shim_dir).expect("create shim dir");
    let shim_path = shim_dir.join("duckdb");
    let marker = shell_single_quote(&root.display().to_string());
    let capture = shell_single_quote(&capture_path.display().to_string());
    let real = shell_single_quote(&real_duckdb);
    let script = format!(
        "#!/bin/sh\n\
         case \" $* \" in\n\
           *'{marker}'*)\n\
             case \" $* \" in\n\
               *' -c '*)\n\
                 case \" $* \" in\n\
                   *'code_symbols'*) printf '{code_symbol_probe_row_count}\\n' ;;\n\
                   *) printf '{section_probe_row_count}\\n' ;;\n\
                 esac\n\
                 exit 0\n\
                 ;;\n\
               *) cat > '{capture}'; : > \"$1\"; exit 0 ;;\n\
             esac\n\
             ;;\n\
         esac\n\
         if [ -n '{real}' ]; then exec '{real}' \"$@\"; fi\n\
         exit 127\n"
    );
    std::fs::write(&shim_path, script).expect("write duckdb shim");
    std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod duckdb shim");
    let joined_path = std::env::join_paths(
        std::iter::once(shim_dir.clone()).chain(std::env::split_paths(&original_path)),
    )
    .expect("join PATH");
    std::env::set_var("PATH", joined_path);

    let build_result = analyst::build(
        root,
        AnalystBuildOptions {
            artifact_dir: Some(artifact_dir.to_path_buf()),
            db_path: Some(db_path.to_path_buf()),
            quiet: true,
        },
    );

    std::env::set_var("PATH", original_path);
    build_result.expect("analyst build with fake duckdb");
    assert!(
        db_path.is_file(),
        "fake duckdb should leave a materialized DB placeholder"
    );
    std::fs::read_to_string(&capture_path).expect("read captured analyst SQL")
}

#[test]
fn analyst_build_emits_temporal_and_diagnostics_views_when_parquets_exist() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = temporal_artifact("analyst-temporal-views");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    let db_path = tempdir.path().join("analyst.duckdb");

    if !build_analyst_or_skip(tempdir.path(), &artifact_dir, &db_path) {
        return;
    }

    let counts = query_csv(
        &db_path,
        "SELECT commit_count, symbol_snapshot_count, temporal_edge_count, diagnostic_count
         FROM _meta;",
    );
    assert_eq!(counts.trim(), "1,1,2,1");

    let renamed_from_snapshots = query_csv(
        &db_path,
        "SELECT count(*)
         FROM temporal_edges
         WHERE source_kind = 'snapshot'
           AND change_kind LIKE 'renamed_from%';",
    );
    assert_eq!(renamed_from_snapshots.trim(), "1");
}

#[test]
fn analyst_build_skips_temporal_and_diagnostics_views_without_optional_parquets() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = structural_artifact("analyst-structural-only");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    let db_path = tempdir.path().join("analyst.duckdb");

    if !build_analyst_or_skip(tempdir.path(), &artifact_dir, &db_path) {
        return;
    }

    let optional_views = query_csv(
        &db_path,
        "SELECT view_name
         FROM duckdb_views()
         WHERE view_name IN ('commits', 'symbol_snapshots', 'temporal_edges', 'diagnostics')
         ORDER BY view_name;",
    );
    assert_eq!(optional_views.trim(), "");
}

#[test]
fn analyst_build_emits_external_dependency_surface_views() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = external_dependency_artifact("analyst-external-dependencies");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    let db_path = tempdir.path().join("analyst.duckdb");

    if !build_analyst_or_skip(tempdir.path(), &artifact_dir, &db_path) {
        return;
    }

    let dependency_views = query_csv(
        &db_path,
        "SELECT view_name
         FROM duckdb_views()
         WHERE view_name IN ('external_nodes', 'v_dependency_surface')
         ORDER BY view_name;",
    );
    assert_eq!(
        dependency_views.trim(),
        "external_nodes\nv_dependency_surface"
    );

    let origins = query_csv(
        &db_path,
        "SELECT qualified_name, origin
         FROM external_nodes
         ORDER BY qualified_name;",
    );
    assert_eq!(
        origins.trim(),
        "alloc::vec::Vec,std\ncore::fmt::Formatter,std\nserde::Serialize,serde\nstd::fmt::Debug,std"
    );

    let surface = query_csv(
        &db_path,
        "SELECT crate_name, file_path, external_origin, external_symbol, inbound_import_count
         FROM v_dependency_surface
         ORDER BY external_origin, external_symbol;",
    );
    assert_eq!(
        surface.trim(),
        "spur-core,crates/spur-core/src/lib.rs,serde,serde::Serialize,2\nspur-core,crates/spur-core/src/lib.rs,std,alloc::vec::Vec,1\nspur-core,crates/spur-core/src/lib.rs,std,core::fmt::Formatter,1\nspur-core,crates/spur-core/src/lib.rs,std,std::fmt::Debug,1"
    );

    let pgq_imports = query_csv(
        &db_path,
        "FROM GRAPH_TABLE (code
           MATCH (s:duckpgq_nodes)-[i:imports]->(e:External)
           WHERE s.file_path = 'crates/spur-core/src/lib.rs'
           COLUMNS (e.qualified_name AS external_symbol)
         )
         ORDER BY external_symbol;",
    );
    assert_eq!(
        pgq_imports.trim(),
        "alloc::vec::Vec\ncore::fmt::Formatter\nserde::Serialize\nstd::fmt::Debug"
    );
}

#[test]
fn analyst_build_emits_cross_crate_call_surface_views() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = cross_crate_call_artifact("analyst-cross-crate-calls");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    let db_path = tempdir.path().join("analyst.duckdb");

    run_duckdb_sql(&db_path, &structural_init_sql_for_test(&artifact_dir));

    let cross_crate_views = query_csv(
        &db_path,
        "SELECT view_name
         FROM duckdb_views()
         WHERE view_name IN ('v_cross_crate_calls', 'v_import_licensed_precision_gate')
         ORDER BY view_name;",
    );
    assert_eq!(
        cross_crate_views.trim(),
        "v_cross_crate_calls\nv_import_licensed_precision_gate"
    );

    let calls = query_csv(
        &db_path,
        "SELECT source_crate, target_crate, source_symbol, target_symbol, bind_method
         FROM v_cross_crate_calls
         ORDER BY source_crate, target_crate, target_symbol;",
    );
    assert_eq!(
        calls.trim(),
        "spur-mcp,spur-graph,handle_submit,build_facts,import_licensed"
    );

    let import_only_not_counted = query_csv(
        &db_path,
        "SELECT count(*)
         FROM v_cross_crate_calls
         WHERE target_symbol = 'import_only';",
    );
    assert_eq!(import_only_not_counted.trim(), "0");

    let gate_violations = query_csv(
        &db_path,
        "SELECT count(*)
         FROM v_import_licensed_precision_gate;",
    );
    assert_eq!(gate_violations.trim(), "0");
}

#[test]
fn analyst_build_rejects_low_direct_symbol_snapshot_coverage() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = divergent_symbol_id_artifact("analyst-low-direct-coverage");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    let db_path = tempdir.path().join("analyst.duckdb");

    analyst::build(
        tempdir.path(),
        AnalystBuildOptions {
            artifact_dir: Some(artifact_dir),
            db_path: Some(db_path.clone()),
            quiet: true,
        },
    )
    .expect("analyst build");

    assert!(
        !db_path.exists(),
        "analyst DB should not be created when distinct symbol snapshot coverage is below 90%"
    );
}

#[test]
fn analyst_views_map_temporal_churn_by_direct_symbol_ids() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = direct_symbol_id_artifact("analyst-direct-symbol-views");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    let db_path = tempdir.path().join("analyst.duckdb");

    if !build_analyst_or_skip(tempdir.path(), &artifact_dir, &db_path) {
        return;
    }

    let coverage = query_csv(
        &db_path,
        "SELECT
           (SELECT COUNT(*) FROM nodes n JOIN symbol_snapshots s USING (stable_symbol_id)),
           (SELECT node_count FROM _meta LIMIT 1);",
    );
    assert_eq!(coverage.trim(), "2,2");

    let churn = query_csv(
        &db_path,
        "SELECT stable_symbol_id, events
         FROM v_symbol_churn_90d
         ORDER BY stable_symbol_id;",
    );
    assert_eq!(churn.trim(), "struct-caller,1\nstruct-target,1");

    let blast = query_csv(
        &db_path,
        "SELECT stable_symbol_id, caller_count, caller_churn_90d, self_churn_90d,
                blast_radius_score > 0
         FROM v_blast_radius
         WHERE stable_symbol_id = 'struct-target';",
    );
    assert_eq!(blast.trim(), "struct-target,1,1,1,true");
}

#[test]
fn analyst_build_uses_complete_lance_sidecar_for_hybrid_search() {
    let tempdir = tempfile::Builder::new()
        .prefix("fake-duckdb-complete-sidecar")
        .tempdir()
        .expect("tempdir");
    write_section_fixture_source(tempdir.path());
    let artifact = temporal_artifact_with_section("analyst-complete-sidecar");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    write_sections_dataset_best_effort_with_options(
        &artifact,
        tempdir.path(),
        &artifact_dir,
        SectionEmbeddingOptions {
            skip_embeddings: true,
            batch_size: 64,
        },
    );
    spur_graph::store::stamp_sidecar_status(
        &artifact_dir,
        GraphArtifactSidecarStatus {
            complete: true,
            row_counts: GraphArtifactSidecarRowCounts {
                section_bodies: 1,
                code_symbols: 1,
            },
        },
    )
    .expect("stamp sidecar complete");
    let db_path = tempdir.path().join("analyst.duckdb");

    let sql = build_analyst_with_fake_duckdb(tempdir.path(), &artifact_dir, &db_path, 1);

    assert!(sql.contains("ATTACH '"));
    assert!(sql.contains("sections.lancedb"));
    assert!(sql.contains("lance_ns.section_bodies"));
    assert!(sql.contains("search_context_candidates_hybrid"));
    assert!(sql.contains("lance_hybrid_search("));
}

#[test]
fn analyst_build_degrades_to_bm25_when_sidecar_manifest_incomplete() {
    let tempdir = tempfile::Builder::new()
        .prefix("fake-duckdb-incomplete-sidecar")
        .tempdir()
        .expect("tempdir");
    write_section_fixture_source(tempdir.path());
    let artifact = temporal_artifact_with_section("analyst-incomplete-sidecar");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    write_sections_dataset_best_effort_with_options(
        &artifact,
        tempdir.path(),
        &artifact_dir,
        SectionEmbeddingOptions {
            skip_embeddings: true,
            batch_size: 64,
        },
    );
    spur_graph::store::stamp_sidecar_status(
        &artifact_dir,
        GraphArtifactSidecarStatus {
            complete: false,
            row_counts: GraphArtifactSidecarRowCounts::default(),
        },
    )
    .expect("stamp sidecar incomplete");
    let db_path = tempdir.path().join("analyst.duckdb");

    let sql = build_analyst_with_fake_duckdb(tempdir.path(), &artifact_dir, &db_path, 1);

    assert!(!sql.contains("ATTACH '"));
    assert!(!sql.contains("lance_ns.section_bodies"));
    assert!(sql.contains("search_context_candidates"));
    assert!(!sql.contains("search_context_candidates_hybrid"));
    assert!(!sql.contains("lance_hybrid_search("));
}

#[test]
fn analyst_build_degrades_to_bm25_when_complete_sidecar_dir_is_empty() {
    let tempdir = tempfile::Builder::new()
        .prefix("fake-duckdb-empty-sidecar")
        .tempdir()
        .expect("tempdir");
    write_section_fixture_source(tempdir.path());
    let artifact = temporal_artifact_with_section("analyst-empty-sidecar");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    std::fs::create_dir_all(artifact_dir.join(SECTIONS_DATASET_DIR))
        .expect("create stale empty sections.lancedb");
    spur_graph::store::stamp_sidecar_status(
        &artifact_dir,
        GraphArtifactSidecarStatus {
            complete: true,
            row_counts: GraphArtifactSidecarRowCounts {
                section_bodies: 1,
                code_symbols: 1,
            },
        },
    )
    .expect("stamp sidecar complete");
    let db_path = tempdir.path().join("analyst.duckdb");

    let sql = build_analyst_with_fake_duckdb(tempdir.path(), &artifact_dir, &db_path, 0);

    assert!(!sql.contains("ATTACH '"));
    assert!(!sql.contains("lance_ns.section_bodies"));
    assert!(sql.contains("search_context_candidates"));
    assert!(!sql.contains("search_context_candidates_hybrid"));
    assert!(!sql.contains("lance_hybrid_search("));
}

#[test]
fn analyst_build_degrades_to_bm25_when_code_symbols_sidecar_missing() {
    let tempdir = tempfile::Builder::new()
        .prefix("fake-duckdb-missing-code-symbol-sidecar")
        .tempdir()
        .expect("tempdir");
    write_section_fixture_source(tempdir.path());
    let artifact = temporal_artifact_with_section("analyst-missing-code-symbol-sidecar");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write artifact");
    write_sections_dataset_best_effort_with_options(
        &artifact,
        tempdir.path(),
        &artifact_dir,
        SectionEmbeddingOptions {
            skip_embeddings: true,
            batch_size: 64,
        },
    );
    std::fs::remove_dir_all(artifact_dir.join(CODE_SYMBOLS_DATASET_DIR))
        .expect("remove stale code_symbols.lance");
    spur_graph::store::stamp_sidecar_status(
        &artifact_dir,
        GraphArtifactSidecarStatus {
            complete: true,
            row_counts: GraphArtifactSidecarRowCounts {
                section_bodies: 1,
                code_symbols: 0,
            },
        },
    )
    .expect("stamp sidecar with missing code symbols");
    let db_path = tempdir.path().join("analyst.duckdb");

    let sql =
        build_analyst_with_fake_duckdb_probe_counts(tempdir.path(), &artifact_dir, &db_path, 1, 0);

    assert!(!sql.contains("ATTACH '"));
    assert!(!sql.contains("lance_ns.section_bodies"));
    assert!(!sql.contains("code_symbols.lance"));
    assert!(sql.contains("search_context_candidates"));
    assert!(!sql.contains("search_context_candidates_hybrid"));
    assert!(!sql.contains("lance_hybrid_search("));
}

fn temporal_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
    let mut artifact = structural_artifact(graph_content_hash);
    let snapshot = SnapshotKey {
        stable_symbol_id: "sym-main".to_string(),
        commit: "c1".to_string(),
    };

    artifact.commits.push(CommitArtifact {
        sha: "c1".to_string(),
        parents: Vec::new(),
        author_time: 1_700_000_001,
        author_name: String::new(),
        author_email: String::new(),
        summary: "initial import".to_string(),
    });
    artifact.symbol_snapshots.push(SymbolSnapshotArtifact {
        key: snapshot.clone(),
        file_path: "src/lib.rs".into(),
        entity_name: "main".to_string(),
        symbol_kind: "function".to_string(),
        enclosing_scope: None,
        byte_range: [0, 10],
        line_range: [1, 1],
        anchor_hash: "anchor-main".to_string(),
        tokens: vec!["main".to_string()],
    });
    artifact.temporal_edges.push(TemporalEdgeArtifact {
        source: EdgeEndpoint::Commit {
            sha: "c1".to_string(),
        },
        target: EdgeEndpoint::Snapshot {
            key: snapshot.clone(),
        },
        relation: RelationKind::Touches,
        parent: None,
        change_kind: Some(ChangeKind::Added),
    });
    artifact.temporal_edges.push(TemporalEdgeArtifact {
        source: EdgeEndpoint::Snapshot {
            key: snapshot.clone(),
        },
        target: EdgeEndpoint::Snapshot {
            key: snapshot.clone(),
        },
        relation: RelationKind::Touches,
        parent: None,
        change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(snapshot))),
    });
    artifact
        .diagnostics
        .push("parse_failed path=src/lib.rs".to_string());
    artifact
}

fn external_dependency_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
    let mut artifact = structural_artifact(graph_content_hash);
    artifact.files = vec![GraphFileArtifact {
        stable_file_id: "file-spur-core-lib".to_string(),
        file_path: "crates/spur-core/src/lib.rs".to_string(),
    }];
    artifact.file_node_ids = vec![NodeId(1)];
    artifact.symbols = vec![
        GraphSymbolArtifact {
            stable_symbol_id: "spur-core-main".to_string(),
            file_path: "crates/spur-core/src/lib.rs".to_string(),
            byte_range: [0, 20],
            line_range: [1, 2],
            entity_name: "main".to_string(),
            qualified_name: "main".to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: "anchor-main".to_string(),
            enclosing_scope: None,
        },
        GraphSymbolArtifact {
            stable_symbol_id: "spur-core-helper".to_string(),
            file_path: "crates/spur-core/src/lib.rs".to_string(),
            byte_range: [30, 50],
            line_range: [4, 5],
            entity_name: "helper".to_string(),
            qualified_name: "helper".to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: "anchor-helper".to_string(),
            enclosing_scope: None,
        },
        external_symbol("external-serde-serialize", "serde::Serialize"),
        external_symbol("external-std-debug", "std::fmt::Debug"),
        external_symbol("external-core-formatter", "core::fmt::Formatter"),
        external_symbol("external-alloc-vec", "alloc::vec::Vec"),
    ];
    artifact.symbol_node_ids = vec![
        NodeId(2),
        NodeId(3),
        NodeId(4),
        NodeId(5),
        NodeId(6),
        NodeId(7),
    ];
    artifact.edges = vec![
        import_edge("spur-core-main", "external-serde-serialize"),
        import_edge("spur-core-helper", "external-serde-serialize"),
        import_edge("spur-core-main", "external-std-debug"),
        import_edge("spur-core-main", "external-core-formatter"),
        import_edge("spur-core-helper", "external-alloc-vec"),
    ];
    artifact
}

fn external_symbol(stable_symbol_id: &str, qualified_name: &str) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: stable_symbol_id.to_string(),
        file_path: qualified_name.to_string(),
        byte_range: [0, 0],
        line_range: [0, 0],
        entity_name: qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(qualified_name)
            .to_string(),
        qualified_name: qualified_name.to_string(),
        symbol_kind: "external".to_string(),
        anchor_hash: format!("anchor-{stable_symbol_id}"),
        enclosing_scope: None,
    }
}

fn import_edge(source: &str, target: &str) -> GraphEdgeArtifact {
    GraphEdgeArtifact {
        source_stable_symbol_id: source.to_string(),
        target_stable_symbol_id: Some(target.to_string()),
        target_label: None,
        relation: RelationKind::Imports,
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
        edge_kind: Some(GraphEdgeKind::ReferencesOther),
        bind_method: Some("external_import".to_string()),
        import_path: None,
        receiver_text: None,
        scope_text: None,
    }
}

fn cross_crate_call_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
    let mut artifact = structural_artifact(graph_content_hash);
    artifact.files = vec![
        GraphFileArtifact {
            stable_file_id: "file-spur-mcp-server".to_string(),
            file_path: "crates/spur-mcp/src/server.rs".to_string(),
        },
        GraphFileArtifact {
            stable_file_id: "file-spur-graph-lib".to_string(),
            file_path: "crates/spur-graph/src/lib.rs".to_string(),
        },
    ];
    artifact.file_node_ids = vec![NodeId(1), NodeId(2)];
    artifact.symbols = vec![
        GraphSymbolArtifact {
            stable_symbol_id: "spur-mcp-handle-submit".to_string(),
            file_path: "crates/spur-mcp/src/server.rs".to_string(),
            byte_range: [0, 20],
            line_range: [1, 2],
            entity_name: "handle_submit".to_string(),
            qualified_name: "spur_mcp::server::handle_submit".to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: "anchor-handle-submit".to_string(),
            enclosing_scope: None,
        },
        GraphSymbolArtifact {
            stable_symbol_id: "spur-graph-build-facts".to_string(),
            file_path: "crates/spur-graph/src/lib.rs".to_string(),
            byte_range: [40, 60],
            line_range: [5, 7],
            entity_name: "build_facts".to_string(),
            qualified_name: "spur_graph::build_facts".to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: "anchor-build-facts".to_string(),
            enclosing_scope: None,
        },
        GraphSymbolArtifact {
            stable_symbol_id: "spur-graph-import-only".to_string(),
            file_path: "crates/spur-graph/src/lib.rs".to_string(),
            byte_range: [80, 100],
            line_range: [10, 12],
            entity_name: "import_only".to_string(),
            qualified_name: "spur_graph::import_only".to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: "anchor-import-only".to_string(),
            enclosing_scope: None,
        },
    ];
    artifact.symbol_node_ids = vec![NodeId(3), NodeId(4), NodeId(5)];
    artifact.edges = vec![
        workspace_import_edge(
            "spur-mcp-handle-submit",
            "spur-graph-build-facts",
            "build_facts",
            "spur_graph::build_facts",
        ),
        workspace_import_edge(
            "spur-mcp-handle-submit",
            "spur-graph-import-only",
            "import_only",
            "spur_graph::import_only",
        ),
        GraphEdgeArtifact {
            source_stable_symbol_id: "spur-mcp-handle-submit".to_string(),
            target_stable_symbol_id: Some("spur-graph-build-facts".to_string()),
            target_label: Some("build_facts".to_string()),
            relation: RelationKind::Calls,
            confidence: Confidence::SyntaxExact,
            confidence_score: 1.0,
            change_kind: None,
            edge_kind: Some(GraphEdgeKind::Calls),
            bind_method: Some("import_licensed".to_string()),
            import_path: None,
            receiver_text: None,
            scope_text: None,
        },
    ];
    artifact
}

fn workspace_import_edge(
    source: &str,
    target: &str,
    target_label: &str,
    import_path: &str,
) -> GraphEdgeArtifact {
    GraphEdgeArtifact {
        source_stable_symbol_id: source.to_string(),
        target_stable_symbol_id: Some(target.to_string()),
        target_label: Some(target_label.to_string()),
        relation: RelationKind::Imports,
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
        edge_kind: Some(GraphEdgeKind::ReferencesOther),
        bind_method: Some("import_path".to_string()),
        import_path: Some(import_path.to_string()),
        receiver_text: None,
        scope_text: None,
    }
}

fn direct_symbol_id_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
    let mut artifact = structural_artifact(graph_content_hash);
    artifact.symbols = vec![
        GraphSymbolArtifact {
            stable_symbol_id: "struct-caller".to_string(),
            file_path: "src/lib.rs".to_string(),
            byte_range: [0, 20],
            line_range: [1, 3],
            entity_name: "caller".to_string(),
            qualified_name: "caller".to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: "anchor-caller".to_string(),
            enclosing_scope: None,
        },
        GraphSymbolArtifact {
            stable_symbol_id: "struct-target".to_string(),
            file_path: "src/lib.rs".to_string(),
            byte_range: [40, 60],
            line_range: [5, 7],
            entity_name: "target".to_string(),
            qualified_name: "target".to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: "anchor-target".to_string(),
            enclosing_scope: None,
        },
    ];
    artifact.symbol_node_ids = vec![NodeId(2), NodeId(3)];
    artifact.edges.push(GraphEdgeArtifact {
        source_stable_symbol_id: "struct-caller".to_string(),
        target_stable_symbol_id: Some("struct-target".to_string()),
        target_label: None,
        relation: RelationKind::Calls,
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
        edge_kind: Some(GraphEdgeKind::Calls),
        bind_method: None,
        import_path: None,
        receiver_text: None,
        scope_text: None,
    });

    artifact.commits.push(CommitArtifact {
        sha: "c1".to_string(),
        parents: Vec::new(),
        author_time: chrono::Utc::now().timestamp(),
        author_name: String::new(),
        author_email: String::new(),
        summary: "touch symbols".to_string(),
    });
    for (structural_id, snapshot_id, name, byte_range, line_range) in [
        ("struct-caller", "struct-caller", "caller", [0, 20], [1, 3]),
        ("struct-target", "struct-target", "target", [40, 60], [5, 7]),
    ] {
        let snapshot = SnapshotKey {
            stable_symbol_id: snapshot_id.to_string(),
            commit: "c1".to_string(),
        };
        artifact.symbol_snapshots.push(SymbolSnapshotArtifact {
            key: snapshot.clone(),
            file_path: "src/lib.rs".into(),
            entity_name: name.to_string(),
            symbol_kind: "function".to_string(),
            enclosing_scope: None,
            byte_range,
            line_range,
            anchor_hash: format!("anchor-{name}"),
            tokens: vec![name.to_string(), structural_id.to_string()],
        });
        artifact.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Commit {
                sha: "c1".to_string(),
            },
            target: EdgeEndpoint::Snapshot { key: snapshot },
            relation: RelationKind::Touches,
            parent: None,
            change_kind: Some(ChangeKind::Modified),
        });
    }

    artifact
}

fn divergent_symbol_id_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
    let mut artifact = direct_symbol_id_artifact(graph_content_hash);
    for snapshot in &mut artifact.symbol_snapshots {
        snapshot.key.stable_symbol_id = format!("snapshot-{}", snapshot.key.stable_symbol_id);
    }
    artifact
}

fn temporal_artifact_with_section(graph_content_hash: &str) -> GraphIndexArtifact {
    let mut artifact = temporal_artifact(graph_content_hash);
    artifact.file_manifests.push(GraphFileManifestEntry {
        stable_file_id: "file-docs-guide".to_string(),
        path: "docs/guide.md".to_string(),
        content_oid: "guide-content".to_string(),
        node_ids: vec![NodeId(10), NodeId(11)],
    });
    artifact.files.push(GraphFileArtifact {
        stable_file_id: "file-docs-guide".to_string(),
        file_path: "docs/guide.md".to_string(),
    });
    artifact.file_node_ids.push(NodeId(10));
    artifact.symbols.push(GraphSymbolArtifact {
        stable_symbol_id: "section-guide".to_string(),
        file_path: "docs/guide.md".to_string(),
        byte_range: [0, SECTION_FIXTURE_BODY.len()],
        line_range: [1, 3],
        entity_name: "Guide".to_string(),
        qualified_name: "Guide".to_string(),
        symbol_kind: "section".to_string(),
        anchor_hash: "anchor-guide".to_string(),
        enclosing_scope: None,
    });
    artifact.symbol_node_ids.push(NodeId(11));
    artifact.edges.push(GraphEdgeArtifact {
        source_stable_symbol_id: "file-docs-guide".to_string(),
        target_stable_symbol_id: Some("section-guide".to_string()),
        target_label: None,
        relation: RelationKind::Contains,
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
        edge_kind: Some(GraphEdgeKind::ReferencesOther),
        bind_method: None,
        import_path: None,
        receiver_text: None,
        scope_text: None,
    });
    artifact
}

const SECTION_FIXTURE_BODY: &str = "# Guide\n\nUse the analyst search fixture.\n";

fn write_section_fixture_source(root: &Path) {
    std::fs::create_dir_all(root.join("docs")).expect("create docs");
    std::fs::write(root.join("docs/guide.md"), SECTION_FIXTURE_BODY).expect("write guide");
}

fn structural_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "spur-graph-phase2".to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "test-manifest-version".to_string(),
        graph_content_hash: graph_content_hash.to_string(),
        file_manifests: Vec::new(),
        files: vec![GraphFileArtifact {
            stable_file_id: "file-src-lib".to_string(),
            file_path: "src/lib.rs".to_string(),
        }],
        file_node_ids: vec![NodeId(1)],
        symbols: Vec::new(),
        symbol_node_ids: Vec::new(),
        edges: Vec::new(),
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}
