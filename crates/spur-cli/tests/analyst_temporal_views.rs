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
fn analyst_bridge_maps_temporal_churn_to_structural_symbols() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = bridge_artifact("analyst-bridge-views");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
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

    let bridge = query_csv(
        &db_path,
        "SELECT structural_stable_symbol_id, snapshot_stable_symbol_id
         FROM v_symbol_id_bridge
         ORDER BY structural_stable_symbol_id;",
    );
    assert_eq!(
        bridge.trim(),
        "struct-caller,snap-caller\nstruct-target,snap-target"
    );

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

fn bridge_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
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
    });

    artifact.commits.push(CommitArtifact {
        sha: "c1".to_string(),
        parents: Vec::new(),
        author_time: chrono::Utc::now().timestamp(),
        summary: "touch symbols".to_string(),
    });
    for (structural_id, snapshot_id, name, byte_range, line_range) in [
        ("struct-caller", "snap-caller", "caller", [0, 20], [1, 3]),
        ("struct-target", "snap-target", "target", [40, 60], [5, 7]),
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
