use std::path::Path;
use std::process::Command;

use spur_cli::commands::analyst::{self, AnalystBuildOptions};
use spur_graph::{
    ChangeKind, CommitArtifact, Confidence, EdgeEndpoint, GraphEdgeArtifact, GraphEdgeKind,
    GraphFileArtifact, GraphIndexArtifact, GraphIndexHeader, GraphSymbolArtifact, NodeId,
    RelationKind, RenamePrev, SnapshotKey, SymbolSnapshotArtifact, TemporalEdgeArtifact,
    WriteOptions,
};

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
