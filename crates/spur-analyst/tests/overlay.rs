use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use spur_analyst::mcp::open_worktree_overlay;
use spur_graph::store::{write_artifact_parquet, write_worktree_delta, WriteOptions};
use spur_graph::{artifact_from_facts, build_facts};

#[test]
fn worktree_overlay_merges_delta_nodes_and_hides_tombstones() -> anyhow::Result<()> {
    let tempdir = tempfile::tempdir()?;
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n")?;
    fs::write(root.join("src/keep.rs"), "pub fn keep() {}\n")?;

    let prev = artifact_from_facts(&build_facts(&root, None)?.0, &root)?;
    let base_artifact_dir = tempdir.path().join("base-artifact");
    write_artifact_parquet(
        &prev,
        &base_artifact_dir,
        WriteOptions::default(),
        Vec::new(),
    )?;

    let base_db_path = tempdir.path().join("analyst.duckdb");
    seed_base_analyst_db(&base_db_path, &base_artifact_dir)?;
    let base_max_dense_id = base_max_dense_id(&base_db_path)?;

    fs::write(root.join("src/a.rs"), "pub fn beta() {}\n")?;
    let delta_dir = tempdir.path().join("delta");
    write_worktree_delta(&prev, &root, &delta_dir)?;

    let overlay = open_worktree_overlay(&base_db_path, &delta_dir)?;
    let rows = node_rows(&overlay)?;

    assert!(
        !rows.contains_key("alpha"),
        "tombstoned base symbol should be absent from merged nodes"
    );
    assert!(
        rows.contains_key("keep"),
        "unmodified base symbol should remain visible"
    );
    let beta_dense_id = rows
        .get("beta")
        .copied()
        .expect("delta symbol should be visible in merged nodes");
    assert!(
        beta_dense_id > base_max_dense_id,
        "delta dense_id {beta_dense_id} should be greater than base max {base_max_dense_id}"
    );

    Ok(())
}

#[test]
fn worktree_overlay_supersedes_base_rows_reemitted_by_delta() -> anyhow::Result<()> {
    let tempdir = tempfile::tempdir()?;
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src/a.rs"),
        "pub fn helper() {}\npub fn alpha() { helper(); missing(); }\n",
    )?;

    let prev = artifact_from_facts(&build_facts(&root, None)?.0, &root)?;
    let base_artifact_dir = tempdir.path().join("base-artifact");
    write_artifact_parquet(
        &prev,
        &base_artifact_dir,
        WriteOptions::default(),
        Vec::new(),
    )?;

    let base_db_path = tempdir.path().join("analyst.duckdb");
    seed_base_analyst_db(&base_db_path, &base_artifact_dir)?;

    fs::write(
        root.join("src/a.rs"),
        "pub fn helper() {}\npub fn alpha() { helper(); missing(); let _x = 1; }\n",
    )?;
    let delta_dir = tempdir.path().join("delta");
    write_worktree_delta(&prev, &root, &delta_dir)?;

    let overlay = open_worktree_overlay(&base_db_path, &delta_dir)?;

    assert_eq!(
        node_count(&overlay, "helper")?,
        1,
        "unchanged sibling symbols re-emitted by the delta should replace the base row"
    );
    assert_eq!(
        node_count(&overlay, "alpha")?,
        1,
        "changed symbols with stable IDs re-emitted by the delta should replace the base row"
    );
    assert_eq!(
        edge_count(&overlay, "edges", "alpha", "helper")?,
        1,
        "resolved edges re-emitted by the delta should replace the base row"
    );
    assert_eq!(
        edge_count(&overlay, "edges_by_dst", "alpha", "helper")?,
        1,
        "edges_by_dst rows re-emitted by the delta should replace the base row"
    );
    assert_eq!(
        unresolved_source_count(&overlay, "alpha")?,
        1,
        "unresolved edges re-emitted by the delta should replace the base row"
    );

    Ok(())
}

#[test]
fn worktree_overlay_keeps_base_edges_to_reemitted_delta_targets() -> anyhow::Result<()> {
    let tempdir = tempfile::tempdir()?;
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/lib.rs"), "pub mod a;\npub mod b;\n")?;
    fs::write(
        root.join("src/a.rs"),
        "pub fn helper() {}\npub fn other() {}\n",
    )?;
    fs::write(
        root.join("src/b.rs"),
        "pub fn gamma() { crate::a::helper(); }\n",
    )?;

    let prev = artifact_from_facts(&build_facts(&root, None)?.0, &root)?;
    let base_artifact_dir = tempdir.path().join("base-artifact");
    write_artifact_parquet(
        &prev,
        &base_artifact_dir,
        WriteOptions::default(),
        Vec::new(),
    )?;

    let base_db_path = tempdir.path().join("analyst.duckdb");
    seed_base_analyst_db(&base_db_path, &base_artifact_dir)?;

    fs::write(
        root.join("src/a.rs"),
        "pub fn helper() {}\npub fn other() { let _x = 1; }\n",
    )?;
    let delta_dir = tempdir.path().join("delta");
    write_worktree_delta(&prev, &root, &delta_dir)?;

    let overlay = open_worktree_overlay(&base_db_path, &delta_dir)?;

    assert_eq!(
        node_count(&overlay, "helper")?,
        1,
        "target symbol re-emitted by the delta should still deduplicate with its base row"
    );
    assert_eq!(
        edge_count(&overlay, "edges", "gamma", "helper")?,
        1,
        "base edges from untouched source files should survive when only the target is re-emitted"
    );
    assert_eq!(
        edge_count(&overlay, "edges_by_dst", "gamma", "helper")?,
        1,
        "base edges_by_dst rows from untouched source files should survive when only the target is re-emitted"
    );

    Ok(())
}

#[test]
fn worktree_overlay_hides_symbols_and_edges_from_removed_files() -> anyhow::Result<()> {
    let tempdir = tempfile::tempdir()?;
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src/deleted.rs"),
        "pub fn helper() {}\npub fn alpha() { helper(); }\n",
    )?;
    fs::write(root.join("src/keep.rs"), "pub fn keep() {}\n")?;

    let prev = artifact_from_facts(&build_facts(&root, None)?.0, &root)?;
    let base_artifact_dir = tempdir.path().join("base-artifact");
    write_artifact_parquet(
        &prev,
        &base_artifact_dir,
        WriteOptions::default(),
        Vec::new(),
    )?;

    let base_db_path = tempdir.path().join("analyst.duckdb");
    seed_base_analyst_db(&base_db_path, &base_artifact_dir)?;

    fs::remove_file(root.join("src/deleted.rs"))?;
    let delta_dir = tempdir.path().join("delta");
    write_worktree_delta(&prev, &root, &delta_dir)?;

    let overlay = open_worktree_overlay(&base_db_path, &delta_dir)?;

    assert_eq!(
        node_count(&overlay, "alpha")?,
        0,
        "symbols from a removed file should be hidden by the file tombstone"
    );
    assert_eq!(
        node_count(&overlay, "helper")?,
        0,
        "all symbols from a removed file should be hidden by the file tombstone"
    );
    assert_eq!(
        edge_count(&overlay, "edges", "alpha", "helper")?,
        0,
        "resolved edges from removed-file symbols should be hidden"
    );
    assert_eq!(
        edge_count(&overlay, "edges_by_dst", "alpha", "helper")?,
        0,
        "edges_by_dst rows from removed-file symbols should be hidden"
    );

    Ok(())
}

fn seed_base_analyst_db(db_path: &Path, artifact_dir: &Path) -> anyhow::Result<()> {
    let conn = duckdb::Connection::open(db_path)?;
    let artifact_dir = sql_escape_path(artifact_dir);
    conn.execute_batch(&format!(
        r#"
        CREATE OR REPLACE TABLE node_dense_id_map AS
        WITH referenced_ids AS (
          SELECT stable_symbol_id FROM read_parquet('{artifact_dir}/nodes.parquet')
          UNION
          SELECT source_stable_id AS stable_symbol_id FROM read_parquet('{artifact_dir}/edges.parquet')
          UNION
          SELECT target_stable_id FROM read_parquet('{artifact_dir}/edges.parquet')
          UNION
          SELECT source_stable_id FROM read_parquet('{artifact_dir}/edges_by_dst.parquet')
          UNION
          SELECT target_stable_id FROM read_parquet('{artifact_dir}/edges_by_dst.parquet')
          UNION
          SELECT source_stable_id FROM read_parquet('{artifact_dir}/edges_unresolved.parquet')
        )
        SELECT
          stable_symbol_id,
          ROW_NUMBER() OVER (ORDER BY stable_symbol_id) AS dense_id
        FROM (
          SELECT DISTINCT stable_symbol_id
          FROM referenced_ids
          WHERE stable_symbol_id IS NOT NULL
        );

        CREATE OR REPLACE VIEW nodes AS
        SELECT n.* REPLACE (m.dense_id AS node_id)
        FROM read_parquet('{artifact_dir}/nodes.parquet') n
        JOIN node_dense_id_map m USING (stable_symbol_id);

        CREATE OR REPLACE VIEW edges AS
        SELECT e.* REPLACE (
          s.dense_id AS src_id,
          d.dense_id AS dst_id
        )
        FROM read_parquet('{artifact_dir}/edges.parquet') e
        JOIN node_dense_id_map s ON s.stable_symbol_id = e.source_stable_id
        JOIN node_dense_id_map d ON d.stable_symbol_id = e.target_stable_id;

        CREATE OR REPLACE VIEW edges_by_dst AS
        SELECT e.* REPLACE (
          s.dense_id AS src_id,
          d.dense_id AS dst_id
        )
        FROM read_parquet('{artifact_dir}/edges_by_dst.parquet') e
        JOIN node_dense_id_map s ON s.stable_symbol_id = e.source_stable_id
        JOIN node_dense_id_map d ON d.stable_symbol_id = e.target_stable_id;

        CREATE OR REPLACE VIEW edges_unresolved AS
        SELECT e.* REPLACE (s.dense_id AS src_id)
        FROM read_parquet('{artifact_dir}/edges_unresolved.parquet') e
        JOIN node_dense_id_map s ON s.stable_symbol_id = e.source_stable_id;

        CREATE OR REPLACE VIEW files AS
        SELECT *
        FROM read_parquet('{artifact_dir}/files.parquet');

        CREATE OR REPLACE VIEW file_manifests AS
        SELECT *
        FROM read_parquet('{artifact_dir}/file_manifests.parquet');

        CREATE OR REPLACE VIEW tombstones AS
        SELECT *
        FROM read_parquet('{artifact_dir}/tombstones.parquet');
        "#
    ))?;
    Ok(())
}

fn base_max_dense_id(db_path: &Path) -> anyhow::Result<i64> {
    let conn = duckdb::Connection::open(db_path)?;
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(dense_id), 0) FROM node_dense_id_map",
        [],
        |row| row.get(0),
    )?)
}

fn node_rows(conn: &duckdb::Connection) -> anyhow::Result<BTreeMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT entity_name, node_id FROM nodes")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut by_name = BTreeMap::new();
    for row in rows {
        let (entity_name, node_id) = row?;
        by_name.insert(entity_name, node_id);
    }
    Ok(by_name)
}

fn node_count(conn: &duckdb::Connection, entity_name: &str) -> anyhow::Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM nodes WHERE entity_name = ?",
        duckdb::params![entity_name],
        |row| row.get(0),
    )?)
}

fn edge_count(
    conn: &duckdb::Connection,
    view: &str,
    source_entity_name: &str,
    target_entity_name: &str,
) -> anyhow::Result<i64> {
    let sql = format!(
        r#"
        SELECT COUNT(*)
        FROM {view} e
        JOIN base.nodes s ON s.stable_symbol_id = e.source_stable_id
        JOIN base.nodes d ON d.stable_symbol_id = e.target_stable_id
        WHERE s.entity_name = ?
          AND d.entity_name = ?
        "#
    );
    Ok(conn.query_row(
        &sql,
        duckdb::params![source_entity_name, target_entity_name],
        |row| row.get(0),
    )?)
}

fn unresolved_source_count(
    conn: &duckdb::Connection,
    source_entity_name: &str,
) -> anyhow::Result<i64> {
    Ok(conn.query_row(
        r#"
        SELECT COUNT(*)
        FROM edges_unresolved e
        JOIN base.nodes s ON s.stable_symbol_id = e.source_stable_id
        WHERE s.entity_name = ?
        "#,
        duckdb::params![source_entity_name],
        |row| row.get(0),
    )?)
}

fn sql_escape_path(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}
