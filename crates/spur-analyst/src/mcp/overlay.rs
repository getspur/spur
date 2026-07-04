use std::path::Path;

use anyhow::{Context as _, Result};

/// Open an in-memory `DuckDB` connection that overlays a worktree delta on top of
/// a read-only base analyst database.
pub fn open_worktree_overlay(base_path: &Path, delta_dir: &Path) -> Result<duckdb::Connection> {
    let base_path = base_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", base_path.display()))?;
    let delta_dir = delta_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", delta_dir.display()))?;

    let conn = duckdb::Connection::open_in_memory()
        .context("failed to open in-memory DuckDB overlay connection")?;
    attach_base_read_only(&conn, &base_path)?;
    create_overlay_views(&conn, &delta_dir)?;
    Ok(conn)
}

fn attach_base_read_only(conn: &duckdb::Connection, base_path: &Path) -> Result<()> {
    conn.execute_batch(&format!(
        "ATTACH '{}' AS base (READ_ONLY);",
        sql_escape_path(base_path)
    ))
    .with_context(|| {
        format!(
            "failed to attach base analyst DuckDB read-only at {}",
            base_path.display()
        )
    })
}

fn create_overlay_views(conn: &duckdb::Connection, delta_dir: &Path) -> Result<()> {
    let nodes_path = delta_path(delta_dir, "nodes.parquet");
    let edges_path = delta_path(delta_dir, "edges.parquet");
    let edges_by_dst_path = delta_edges_by_dst_path(delta_dir);
    let edges_unresolved_path = delta_path(delta_dir, "edges_unresolved.parquet");
    let files_path = delta_path(delta_dir, "files.parquet");
    let file_manifests_path = delta_path(delta_dir, "file_manifests.parquet");
    let tombstones_path = delta_path(delta_dir, "tombstones.parquet");

    conn.execute_batch(&format!(
        r"
        CREATE OR REPLACE TABLE delta_dense_id_map AS
        WITH referenced_ids AS (
          SELECT stable_symbol_id FROM read_parquet('{nodes_path}')
          UNION
          SELECT source_stable_id AS stable_symbol_id FROM read_parquet('{edges_path}')
          UNION
          SELECT target_stable_id FROM read_parquet('{edges_path}')
          UNION
          SELECT source_stable_id FROM read_parquet('{edges_by_dst_path}')
          UNION
          SELECT target_stable_id FROM read_parquet('{edges_by_dst_path}')
          UNION
          SELECT source_stable_id FROM read_parquet('{edges_unresolved_path}')
        )
        SELECT
          stable_symbol_id,
          (SELECT COALESCE(MAX(dense_id), 0) FROM base.node_dense_id_map)
            + ROW_NUMBER() OVER (ORDER BY stable_symbol_id) AS dense_id
        FROM (
          SELECT DISTINCT stable_symbol_id
          FROM referenced_ids
          WHERE stable_symbol_id IS NOT NULL
        );

        CREATE OR REPLACE VIEW delta_node_ids AS
        SELECT stable_symbol_id
        FROM read_parquet('{nodes_path}')
        WHERE stable_symbol_id IS NOT NULL;

        CREATE OR REPLACE VIEW raw_tombstone_ids AS
        SELECT stable_file_id AS stable_symbol_id
        FROM base.tombstones
        WHERE stable_file_id IS NOT NULL
        UNION
        SELECT stable_file_id AS stable_symbol_id
        FROM read_parquet('{tombstones_path}')
        WHERE stable_file_id IS NOT NULL;

        CREATE OR REPLACE VIEW removed_file_paths AS
        SELECT DISTINCT fm.path
        FROM base.file_manifests fm
        WHERE fm.stable_file_id IN (SELECT stable_symbol_id FROM raw_tombstone_ids)
          AND fm.path NOT IN (SELECT path FROM read_parquet('{file_manifests_path}'));

        CREATE OR REPLACE VIEW tombstone_ids AS
        SELECT stable_symbol_id
        FROM raw_tombstone_ids
        UNION
        SELECT stable_symbol_id
        FROM base.nodes
        WHERE file_path IN (SELECT path FROM removed_file_paths);

        CREATE OR REPLACE VIEW tombstones AS
        SELECT *
        FROM base.tombstones
        UNION ALL
        SELECT *
        FROM read_parquet('{tombstones_path}');

        CREATE OR REPLACE VIEW nodes AS
        SELECT *
        FROM base.nodes
        WHERE stable_symbol_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND stable_symbol_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT n.* REPLACE (m.dense_id AS node_id)
        FROM read_parquet('{nodes_path}') n
        JOIN delta_dense_id_map m USING (stable_symbol_id);

        CREATE OR REPLACE VIEW edges AS
        SELECT *
        FROM base.edges
        WHERE source_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND target_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND source_stable_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT e.* REPLACE (
          COALESCE(src_delta.dense_id, src_base.dense_id) AS src_id,
          COALESCE(dst_delta.dense_id, dst_base.dense_id) AS dst_id
        )
        FROM read_parquet('{edges_path}') e
        LEFT JOIN delta_dense_id_map src_delta
          ON src_delta.stable_symbol_id = e.source_stable_id
        LEFT JOIN base.node_dense_id_map src_base
          ON src_base.stable_symbol_id = e.source_stable_id
        LEFT JOIN delta_dense_id_map dst_delta
          ON dst_delta.stable_symbol_id = e.target_stable_id
        LEFT JOIN base.node_dense_id_map dst_base
          ON dst_base.stable_symbol_id = e.target_stable_id
        WHERE COALESCE(src_delta.dense_id, src_base.dense_id) IS NOT NULL
          AND COALESCE(dst_delta.dense_id, dst_base.dense_id) IS NOT NULL;

        CREATE OR REPLACE VIEW edges_by_dst AS
        SELECT *
        FROM base.edges_by_dst
        WHERE source_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND target_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND source_stable_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT e.* REPLACE (
          COALESCE(src_delta.dense_id, src_base.dense_id) AS src_id,
          COALESCE(dst_delta.dense_id, dst_base.dense_id) AS dst_id
        )
        FROM read_parquet('{edges_by_dst_path}') e
        LEFT JOIN delta_dense_id_map src_delta
          ON src_delta.stable_symbol_id = e.source_stable_id
        LEFT JOIN base.node_dense_id_map src_base
          ON src_base.stable_symbol_id = e.source_stable_id
        LEFT JOIN delta_dense_id_map dst_delta
          ON dst_delta.stable_symbol_id = e.target_stable_id
        LEFT JOIN base.node_dense_id_map dst_base
          ON dst_base.stable_symbol_id = e.target_stable_id
        WHERE COALESCE(src_delta.dense_id, src_base.dense_id) IS NOT NULL
          AND COALESCE(dst_delta.dense_id, dst_base.dense_id) IS NOT NULL;

        CREATE OR REPLACE VIEW edges_unresolved AS
        SELECT *
        FROM base.edges_unresolved
        WHERE source_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND source_stable_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT e.* REPLACE (
          COALESCE(src_delta.dense_id, src_base.dense_id) AS src_id
        )
        FROM read_parquet('{edges_unresolved_path}') e
        LEFT JOIN delta_dense_id_map src_delta
          ON src_delta.stable_symbol_id = e.source_stable_id
        LEFT JOIN base.node_dense_id_map src_base
          ON src_base.stable_symbol_id = e.source_stable_id
        WHERE COALESCE(src_delta.dense_id, src_base.dense_id) IS NOT NULL;

        CREATE OR REPLACE VIEW files AS
        SELECT *
        FROM base.files
        WHERE stable_file_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND file_path NOT IN (SELECT file_path FROM read_parquet('{files_path}'))
        UNION ALL
        SELECT *
        FROM read_parquet('{files_path}');

        CREATE OR REPLACE VIEW file_manifests AS
        SELECT *
        FROM base.file_manifests
        WHERE stable_file_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND path NOT IN (SELECT path FROM read_parquet('{file_manifests_path}'))
        UNION ALL
        SELECT *
        FROM read_parquet('{file_manifests_path}');
        "
    ))
    .with_context(|| {
        format!(
            "failed to create worktree overlay views for delta {}",
            delta_dir.display()
        )
    })
}

fn delta_path(delta_dir: &Path, file_name: &str) -> String {
    sql_escape_path(&delta_dir.join(file_name))
}

fn delta_edges_by_dst_path(delta_dir: &Path) -> String {
    let path = delta_dir.join("edges_by_dst.parquet");
    let path = if path.exists() {
        path
    } else {
        delta_dir.join("edges.parquet")
    };
    sql_escape_path(&path)
}

fn sql_escape_path(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}
