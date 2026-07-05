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

struct OverlayDeltaPaths {
    nodes: String,
    edges: String,
    edges_by_dst: String,
    edges_unresolved: String,
    files: String,
    file_manifests: String,
    tombstones: String,
}

impl OverlayDeltaPaths {
    fn from_delta_dir(delta_dir: &Path) -> Self {
        Self {
            nodes: delta_path(delta_dir, "nodes.parquet"),
            edges: delta_path(delta_dir, "edges.parquet"),
            edges_by_dst: delta_edges_by_dst_path(delta_dir),
            edges_unresolved: delta_path(delta_dir, "edges_unresolved.parquet"),
            files: delta_path(delta_dir, "files.parquet"),
            file_manifests: delta_path(delta_dir, "file_manifests.parquet"),
            tombstones: delta_path(delta_dir, "tombstones.parquet"),
        }
    }
}

fn create_overlay_views(conn: &duckdb::Connection, delta_dir: &Path) -> Result<()> {
    let paths = OverlayDeltaPaths::from_delta_dir(delta_dir);
    create_overlay_view_batches(conn, &paths).with_context(|| {
        format!(
            "failed to create worktree overlay views for delta {}",
            delta_dir.display()
        )
    })
}

fn create_overlay_view_batches(conn: &duckdb::Connection, paths: &OverlayDeltaPaths) -> Result<()> {
    create_overlay_id_views(conn, paths)?;
    create_overlay_tombstone_views(conn, paths)?;
    create_overlay_node_view(conn, paths)?;
    create_resolved_edge_view(conn, "edges", &paths.edges)?;
    create_resolved_edge_view(conn, "edges_by_dst", &paths.edges_by_dst)?;
    create_unresolved_edge_view(conn, paths)?;
    create_file_views(conn, paths)?;
    Ok(())
}

fn create_overlay_id_views(conn: &duckdb::Connection, paths: &OverlayDeltaPaths) -> Result<()> {
    let nodes_path = paths.nodes.as_str();
    let edges_path = paths.edges.as_str();
    let edges_by_dst_path = paths.edges_by_dst.as_str();
    let edges_unresolved_path = paths.edges_unresolved.as_str();

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
        "
    ))?;
    Ok(())
}

fn create_overlay_tombstone_views(
    conn: &duckdb::Connection,
    paths: &OverlayDeltaPaths,
) -> Result<()> {
    let file_manifests_path = paths.file_manifests.as_str();
    let tombstones_path = paths.tombstones.as_str();

    conn.execute_batch(&format!(
        r"
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
        "
    ))?;
    Ok(())
}

fn create_overlay_node_view(conn: &duckdb::Connection, paths: &OverlayDeltaPaths) -> Result<()> {
    let nodes_path = paths.nodes.as_str();

    conn.execute_batch(&format!(
        r"
        CREATE OR REPLACE VIEW nodes AS
        SELECT *
        FROM base.nodes
        WHERE stable_symbol_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND stable_symbol_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT n.* REPLACE (m.dense_id AS node_id)
        FROM read_parquet('{nodes_path}') n
        JOIN delta_dense_id_map m USING (stable_symbol_id);
        "
    ))?;
    Ok(())
}

fn create_resolved_edge_view(
    conn: &duckdb::Connection,
    view_name: &str,
    parquet_path: &str,
) -> Result<()> {
    conn.execute_batch(&format!(
        r"
        CREATE OR REPLACE VIEW {view_name} AS
        SELECT *
        FROM base.{view_name}
        WHERE source_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND target_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND source_stable_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT e.* REPLACE (
          COALESCE(src_delta.dense_id, src_base.dense_id) AS src_id,
          COALESCE(dst_delta.dense_id, dst_base.dense_id) AS dst_id
        )
        FROM read_parquet('{parquet_path}') e
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
        "
    ))?;
    Ok(())
}

fn create_unresolved_edge_view(conn: &duckdb::Connection, paths: &OverlayDeltaPaths) -> Result<()> {
    let edges_unresolved_path = paths.edges_unresolved.as_str();

    conn.execute_batch(&format!(
        r"
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
        "
    ))?;
    Ok(())
}

fn create_file_views(conn: &duckdb::Connection, paths: &OverlayDeltaPaths) -> Result<()> {
    let files_path = paths.files.as_str();
    let file_manifests_path = paths.file_manifests.as_str();

    conn.execute_batch(&format!(
        r"
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
    ))?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{sql_escape_path, OverlayDeltaPaths};

    #[test]
    fn overlay_delta_paths_fall_back_to_edges_when_reverse_edges_are_absent() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let delta_dir = tempdir.path();

        let paths = OverlayDeltaPaths::from_delta_dir(delta_dir);
        assert_eq!(
            paths.edges_by_dst,
            sql_escape_path(&delta_dir.join("edges.parquet"))
        );

        std::fs::write(delta_dir.join("edges_by_dst.parquet"), []).expect("write edges_by_dst");
        let paths = OverlayDeltaPaths::from_delta_dir(delta_dir);
        assert_eq!(
            paths.edges_by_dst,
            sql_escape_path(&delta_dir.join("edges_by_dst.parquet"))
        );
    }
}
