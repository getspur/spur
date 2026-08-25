use std::path::Path;

use duckdb::Connection;
use spur_graph::store::lance_sections::SECTIONS_PARQUET;

use crate::mcp::McpHandlerError;

use super::projection::{lede, DocHit};
use super::DocNavigateRequest;

pub(super) fn open_sections_conn(artifact_dir: &Path) -> Result<Connection, McpHandlerError> {
    let parquet_path = artifact_dir.join(SECTIONS_PARQUET);
    if !parquet_path.is_file() {
        return Err(McpHandlerError::NotFound(format!(
            "failed to open sections parquet at `{}`",
            parquet_path.display()
        )));
    }
    let conn = Connection::open_in_memory().map_err(|error| {
        McpHandlerError::Internal(format!("failed to open DuckDB for doc_navigate: {error}"))
    })?;
    let path_sql = parquet_path.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!(
        "CREATE TABLE sections AS\n\
         SELECT stable_symbol_id,\n\
                qualified_name,\n\
                file_path,\n\
                heading_level,\n\
                child_count,\n\
                CAST(body_text AS VARCHAR) AS body_text,\n\
                body_byte_start,\n\
                parent_stable_id\n\
         FROM read_parquet('{path_sql}');"
    ))
    .map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to read sections parquet `{}`: {error}",
            parquet_path.display()
        ))
    })?;
    let _ = conn.execute_batch(
        "INSTALL fts; LOAD fts;\n\
         PRAGMA create_fts_index('sections', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');",
    );
    Ok(conn)
}

pub(super) fn fts_hits(
    conn: &Connection,
    request: &DocNavigateRequest,
) -> Result<Vec<DocHit>, McpHandlerError> {
    let query = request.query.as_deref().unwrap_or_default();
    match conn.prepare(
        "SELECT s.stable_symbol_id, s.qualified_name, s.file_path, s.heading_level,
                s.child_count, s.body_text, s.body_byte_start,
                fts_main_sections.match_bm25(s.stable_symbol_id, ?1) AS score
         FROM sections s
         WHERE fts_main_sections.match_bm25(s.stable_symbol_id, ?1) IS NOT NULL
         ORDER BY score DESC
         LIMIT ?2",
    ) {
        Ok(mut stmt) => {
            let mut rows = stmt
                .query(duckdb::params![query, request.k as i64])
                .map_err(|error| {
                    McpHandlerError::Internal(format!("doc_navigate FTS failed: {error}"))
                })?;
            collect_hits(&mut rows, true)
        }
        Err(_) => {
            let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
            let mut stmt = conn
                .prepare(
                    "SELECT stable_symbol_id, qualified_name, file_path, heading_level,
                            child_count, body_text, body_byte_start, NULL AS score
                     FROM sections
                     WHERE body_text ILIKE ?1
                     LIMIT ?2",
                )
                .map_err(|error| {
                    McpHandlerError::Internal(format!(
                        "failed to prepare doc_navigate body scan: {error}"
                    ))
                })?;
            let mut rows = stmt
                .query(duckdb::params![like, request.k as i64])
                .map_err(|error| {
                    McpHandlerError::Internal(format!("doc_navigate body scan failed: {error}"))
                })?;
            collect_hits(&mut rows, false)
        }
    }
}

pub(super) fn child_hits(conn: &Connection, root: &str) -> Result<Vec<DocHit>, McpHandlerError> {
    let mut stmt = conn
        .prepare(
            "SELECT stable_symbol_id, qualified_name, file_path, heading_level,
                    child_count, body_text, body_byte_start, NULL AS score
             FROM sections
             WHERE parent_stable_id = ?1
             ORDER BY file_path, body_byte_start, stable_symbol_id",
        )
        .map_err(|error| {
            McpHandlerError::Internal(format!(
                "failed to prepare doc_navigate root expansion: {error}"
            ))
        })?;
    let mut rows = stmt.query(duckdb::params![root]).map_err(|error| {
        McpHandlerError::Internal(format!("doc_navigate root expansion failed: {error}"))
    })?;
    collect_hits(&mut rows, false)
}

fn collect_hits(
    rows: &mut duckdb::Rows<'_>,
    include_score: bool,
) -> Result<Vec<DocHit>, McpHandlerError> {
    let mut hits = Vec::new();
    while let Some(row) = rows.next().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read doc_navigate rows: {error}"))
    })? {
        hits.push(DocHit {
            stable_symbol_id: row.get::<_, String>(0).unwrap_or_default(),
            qualified_name: row.get::<_, String>(1).unwrap_or_default(),
            file_path: row.get::<_, String>(2).unwrap_or_default(),
            heading_level: row.get::<_, u8>(3).unwrap_or_default(),
            child_count: row.get::<_, u32>(4).unwrap_or_default(),
            score: if include_score {
                row.get::<_, Option<f32>>(7).ok().flatten()
            } else {
                None
            },
            lede: Some(lede(&row.get::<_, String>(5).unwrap_or_default())),
        });
    }
    Ok(hits)
}
