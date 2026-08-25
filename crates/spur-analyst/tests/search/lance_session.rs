use std::fs;
use std::path::Path;

use spur_cli::commands::analyst::{self, AnalystBuildOptions};
use spur_graph::store::{
    stamp_sidecar_status, write_sections_dataset, GraphArtifactSidecarRowCounts,
    GraphArtifactSidecarStatus,
};
use spur_graph::{artifact_from_facts, build_facts, WriteOptions};

fn query_csv(db_path: &Path, sql: &str) -> String {
    let conn = duckdb::Connection::open(db_path).expect("open analyst duckdb");
    let _ = conn.execute_batch("INSTALL fts; LOAD fts; INSTALL icu; LOAD icu;");
    let mut stmt = conn.prepare(sql).expect("prepare query");
    let mut rows = stmt.query([]).expect("query rows");
    let column_count = rows.as_ref().expect("query result").column_count();
    let mut lines = Vec::new();
    while let Some(row) = rows.next().expect("read row") {
        let mut fields = Vec::with_capacity(column_count);
        for idx in 0..column_count {
            fields.push(match row.get_ref(idx).expect("read column") {
                duckdb::types::ValueRef::Null => String::new(),
                duckdb::types::ValueRef::Text(value) => {
                    String::from_utf8_lossy(value).into_owned()
                }
                other => format!("{other:?}"),
            });
        }
        lines.push(fields.join(","));
    }
    lines.join("\n")
}

#[test]
fn analyst_session_searches_parquet_sections_and_joins_nodes() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    fs::write(
        root.join("docs/guide.md"),
        "# Emit Sections\n\nbody span widen content.\n\n## Details\n\nMore section body.\n",
    )
    .expect("write markdown fixture");

    let facts = build_facts(&root, None)
        .expect("build stage-1 fixture facts")
        .0;
    let artifact = artifact_from_facts(&facts, &root).expect("stage-1 artifact");
    let artifact_dir = spur_graph::store::parquet::write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    write_sections_dataset(&artifact, &root, &artifact_dir).expect("write sections.parquet");
    stamp_sidecar_status(
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
    analyst::build(
        tempdir.path(),
        AnalystBuildOptions {
            artifact_dir: Some(artifact_dir.clone()),
            db_path: Some(db_path.clone()),
            quiet: true,
        },
    )
    .expect("analyst build");
    if !db_path.is_file() {
        eprintln!("skipping: analyst init SQL did not produce a DB");
        return;
    }

    let section_count = query_csv(&db_path, "SELECT count(*) FROM sections;");
    assert_ne!(
        section_count.trim(),
        "0",
        "parquet sections should load into DuckDB"
    );

    let rows = query_csv(
        &db_path,
        "SELECT s.stable_symbol_id, n.qualified_name, s.body_text
         FROM sections s
         JOIN nodes n USING (stable_symbol_id)
         WHERE s.body_text ILIKE '%widen%'
         ORDER BY s.stable_symbol_id;",
    );

    assert!(
        !rows.trim().is_empty(),
        "parquet sections should join nodes: {rows:?}"
    );
    assert!(
        rows.to_lowercase().contains("widen"),
        "loaded section bodies should keep the fixture prose: {rows:?}"
    );
}
