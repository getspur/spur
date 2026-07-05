use std::path::Path;

use anyhow::{anyhow, Context as _, Result};

use crate::{
    api::{
        KnowledgePathEngine, KnowledgePathOptions, KnowledgePathResult, KnowledgePathResultContext,
        KnowledgePathRow, KnowledgePathStatus,
    },
    db::connection::open_analyst_connection_read_only,
};

use super::{
    graph_content_hash, i64_to_usize, path_result, query_duckpgq_direct_paths,
    query_duckpgq_shortest_hops,
    query_sql::{recursive_path_sql, RecursivePathMode},
    unavailable_path_result,
};

pub const MAX_CONTEXT_PATH_HOPS: usize = 6;
pub const MAX_CONTEXT_PATHS: usize = 12;

pub fn query_context_paths(
    db_path: &Path,
    source_stable_id: &str,
    target_stable_id: &str,
    options: KnowledgePathOptions,
) -> Result<KnowledgePathResult> {
    let conn = open_analyst_connection_read_only(db_path)?;
    query_context_paths_with_conn(&conn, db_path, source_stable_id, target_stable_id, options)
}

pub fn query_context_paths_with_conn(
    conn: &duckdb::Connection,
    db_path: &Path,
    source_stable_id: &str,
    target_stable_id: &str,
    options: KnowledgePathOptions,
) -> Result<KnowledgePathResult> {
    let source_stable_id = source_stable_id.trim();
    let target_stable_id = target_stable_id.trim();
    if source_stable_id.is_empty() || target_stable_id.is_empty() {
        return Err(anyhow!(
            "knowledge context path query requires non-empty source and target stable IDs"
        ));
    }

    let max_hops = options.max_hops.clamp(1, MAX_CONTEXT_PATH_HOPS);
    let max_paths = options.max_paths.clamp(1, MAX_CONTEXT_PATHS);

    let result_context = KnowledgePathResultContext {
        db_path,
        graph_content_hash: graph_content_hash(conn),
        max_hops,
        max_paths,
    };
    if source_stable_id == target_stable_id {
        let caveat = "source and target stable IDs are identical; zero-hop paths have no edge rows"
            .to_owned();
        return Ok(path_result(
            &result_context,
            KnowledgePathEngine::RecursiveSql,
            KnowledgePathStatus::PathFound,
            Some(caveat),
            Vec::new(),
        ));
    }

    if options.undirected {
        match query_recursive_undirected_context_path_rows(
            conn,
            source_stable_id,
            target_stable_id,
            max_hops,
            max_paths,
            KnowledgePathEngine::RecursiveSql,
        ) {
            Ok(rows) if rows.is_empty() => {
                let caveat = format!("no undirected path found within {max_hops} hops");
                return Ok(path_result(
                    &result_context,
                    KnowledgePathEngine::RecursiveSql,
                    KnowledgePathStatus::NoPath,
                    Some(caveat),
                    rows,
                ));
            }
            Ok(rows) => {
                return Ok(path_result(
                    &result_context,
                    KnowledgePathEngine::RecursiveSql,
                    KnowledgePathStatus::PathFound,
                    None,
                    rows,
                ));
            }
            Err(error) => {
                let caveat = format!("undirected context path search unavailable: {error:#}");
                return Ok(unavailable_path_result(
                    &result_context,
                    source_stable_id,
                    target_stable_id,
                    caveat,
                ));
            }
        }
    }

    if let Ok(rows) =
        query_duckpgq_direct_paths(conn, source_stable_id, target_stable_id, max_paths)
    {
        if !rows.is_empty() {
            return Ok(path_result(
                &result_context,
                KnowledgePathEngine::DuckPgq,
                KnowledgePathStatus::PathFound,
                None,
                rows,
            ));
        }
    }

    match query_duckpgq_shortest_hops(conn, source_stable_id, target_stable_id, max_hops) {
        Ok(Some(shortest_hops)) => {
            match query_recursive_context_path_rows(
                conn,
                source_stable_id,
                target_stable_id,
                shortest_hops,
                max_paths,
                KnowledgePathEngine::DuckPgq,
            ) {
                Ok(rows) if !rows.is_empty() => {
                    return Ok(path_result(
                        &result_context,
                        KnowledgePathEngine::DuckPgq,
                        KnowledgePathStatus::PathFound,
                        None,
                        rows,
                    ));
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    error = %error,
                    "DuckPGQ path length succeeded but recursive edge expansion failed"
                ),
            }
        }
        Ok(None) => {
            let caveat = format!("no path found within {max_hops} hops");
            return Ok(path_result(
                &result_context,
                KnowledgePathEngine::DuckPgq,
                KnowledgePathStatus::NoPath,
                Some(caveat),
                Vec::new(),
            ));
        }
        Err(error) => tracing::debug!(
            error = %error,
            "DuckPGQ context path query unavailable; falling back to recursive SQL"
        ),
    }

    match query_recursive_context_path_rows(
        conn,
        source_stable_id,
        target_stable_id,
        max_hops,
        max_paths,
        KnowledgePathEngine::RecursiveSql,
    ) {
        Ok(rows) if rows.is_empty() => {
            let caveat = format!("no path found within {max_hops} hops");
            Ok(path_result(
                &result_context,
                KnowledgePathEngine::RecursiveSql,
                KnowledgePathStatus::NoPath,
                Some(caveat),
                rows,
            ))
        }
        Ok(rows) => Ok(path_result(
            &result_context,
            KnowledgePathEngine::RecursiveSql,
            KnowledgePathStatus::PathFound,
            None,
            rows,
        )),
        Err(error) => {
            let caveat = format!("context path search unavailable: {error:#}");
            Ok(unavailable_path_result(
                &result_context,
                source_stable_id,
                target_stable_id,
                caveat,
            ))
        }
    }
}

fn query_recursive_context_path_rows(
    conn: &duckdb::Connection,
    source_stable_id: &str,
    target_stable_id: &str,
    max_hops: usize,
    max_paths: usize,
    engine: KnowledgePathEngine,
) -> Result<Vec<KnowledgePathRow>> {
    let sql = recursive_path_sql(max_hops, max_paths, RecursivePathMode::Directed);
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare recursive context path query")?;
    let rows = stmt
        .query_map(duckdb::params![source_stable_id, target_stable_id], |row| {
            Ok(KnowledgePathRow {
                path_index: i64_to_usize(row.get(0)?),
                hop_index: i64_to_usize(row.get(1)?),
                source_stable_id: row.get(2)?,
                target_stable_id: row.get(3)?,
                relation: row.get(4)?,
                edge_kind: row.get(5)?,
                confidence: row.get(6)?,
                bind_method: row.get(7)?,
                direction: None,
                engine,
                status: KnowledgePathStatus::PathFound,
                caveat: None,
            })
        })
        .context("failed to run recursive context path query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read recursive context path rows")
}

fn query_recursive_undirected_context_path_rows(
    conn: &duckdb::Connection,
    source_stable_id: &str,
    target_stable_id: &str,
    max_hops: usize,
    max_paths: usize,
    engine: KnowledgePathEngine,
) -> Result<Vec<KnowledgePathRow>> {
    let sql = recursive_path_sql(max_hops, max_paths, RecursivePathMode::Undirected);
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare undirected recursive context path query")?;
    let rows = stmt
        .query_map(duckdb::params![source_stable_id, target_stable_id], |row| {
            Ok(KnowledgePathRow {
                path_index: i64_to_usize(row.get(0)?),
                hop_index: i64_to_usize(row.get(1)?),
                source_stable_id: row.get(2)?,
                target_stable_id: row.get(3)?,
                relation: row.get(4)?,
                edge_kind: row.get(5)?,
                confidence: row.get(6)?,
                bind_method: row.get(7)?,
                direction: row.get(8)?,
                engine,
                status: KnowledgePathStatus::PathFound,
                caveat: None,
            })
        })
        .context("failed to run undirected recursive context path query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read undirected recursive context path rows")
}
