use std::fs;
use std::path::Path;
use std::process::Command;

use spur_cli::commands::analyst::{self, AnalystBuildOptions};
use spur_graph::store::write_sections_dataset;
use spur_graph::{artifact_from_facts, build_facts, WriteOptions};

fn duckdb_cli_present() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("duckdb").is_file()))
        .unwrap_or(false)
}

fn query_csv(db_path: &Path, lance_dataset_dir: &Path, sql: &str) -> String {
    let lance_dataset_dir = lance_dataset_dir.display().to_string().replace('\'', "''");
    let sql = format!("LOAD lance;\nATTACH '{lance_dataset_dir}' AS lance_ns (TYPE LANCE);\n{sql}");
    let output = Command::new("duckdb")
        .args(["-csv", "-noheader"])
        .arg(db_path)
        .args(["-c", &sql])
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
fn analyst_session_attaches_lance_sections_and_joins_nodes() {
    if !duckdb_cli_present() {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

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
    write_sections_dataset(&artifact, &root, &artifact_dir).expect("write sections.lancedb");

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
        eprintln!("skipping: duckdb CLI present but analyst init SQL did not produce a DB");
        return;
    }

    let rows = query_csv(
        &db_path,
        &artifact_dir.join(spur_graph::store::SECTIONS_DATASET_DIR),
        "SELECT s.stable_symbol_id, n.qualified_name, s._score
         FROM lance_fts('lance_ns.main.section_bodies', 'body_text',
                        'emit_sections body span widen', k => 10) AS s
         JOIN nodes n USING (stable_symbol_id)
         ORDER BY s._score DESC;",
    );

    assert!(
        !rows.trim().is_empty(),
        "lance_fts join should return at least one section row"
    );
    assert!(
        rows.lines().any(|line| {
            let mut columns = line.split(',');
            columns.next().is_some()
                && columns.next().is_some()
                && columns.next().is_some_and(|score| !score.trim().is_empty())
        }),
        "lance_fts join should include a non-null _score: {rows:?}"
    );
}
