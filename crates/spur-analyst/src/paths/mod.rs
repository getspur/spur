mod context_paths;
mod query_sql;
mod risk_community;

use anyhow::{Context as _, Result};

use crate::{
    api::{
        KnowledgePathEngine, KnowledgePathResult, KnowledgePathResultContext, KnowledgePathRow,
        KnowledgePathStatus,
    },
    db::extensions::load_analyst_duckpgq_extension,
};

use query_sql::{duckpgq_direct_paths_sql, duckpgq_shortest_hops_sql};

pub use context_paths::{
    query_context_paths, query_context_paths_with_conn, MAX_CONTEXT_PATHS, MAX_CONTEXT_PATH_HOPS,
};
pub use risk_community::{
    query_symbol_risk_community, query_symbol_risk_community_with_conn,
    MAX_SYMBOL_RISK_COMMUNITY_IDS,
};

fn graph_content_hash(conn: &duckdb::Connection) -> Option<String> {
    conn.query_row("SELECT graph_content_hash FROM _meta", [], |row| row.get(0))
        .ok()
}

fn i64_to_usize(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
}

fn path_result(
    context: &KnowledgePathResultContext<'_>,
    engine: KnowledgePathEngine,
    status: KnowledgePathStatus,
    caveat: Option<String>,
    rows: Vec<KnowledgePathRow>,
) -> KnowledgePathResult {
    KnowledgePathResult {
        db_path: context.db_path.display().to_string(),
        graph_content_hash: context.graph_content_hash.clone(),
        max_hops: context.max_hops,
        max_paths: context.max_paths,
        engine,
        status,
        caveat,
        rows,
    }
}

fn unavailable_path_result(
    context: &KnowledgePathResultContext<'_>,
    source_stable_id: &str,
    target_stable_id: &str,
    caveat: String,
) -> KnowledgePathResult {
    let row = KnowledgePathRow {
        path_index: 0,
        hop_index: 0,
        source_stable_id: source_stable_id.to_owned(),
        target_stable_id: target_stable_id.to_owned(),
        relation: None,
        edge_kind: None,
        confidence: None,
        bind_method: None,
        direction: None,
        engine: KnowledgePathEngine::Unavailable,
        status: KnowledgePathStatus::Unavailable,
        caveat: Some(caveat.clone()),
    };
    path_result(
        context,
        KnowledgePathEngine::Unavailable,
        KnowledgePathStatus::Unavailable,
        Some(caveat),
        vec![row],
    )
}

fn query_duckpgq_direct_paths(
    conn: &duckdb::Connection,
    source_stable_id: &str,
    target_stable_id: &str,
    max_paths: usize,
) -> Result<Vec<KnowledgePathRow>> {
    load_analyst_duckpgq_extension(conn)?;
    let sql = duckpgq_direct_paths_sql(source_stable_id, target_stable_id, max_paths);
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare DuckPGQ direct path query")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(KnowledgePathRow {
                path_index: 0,
                hop_index: 0,
                source_stable_id: row.get(0)?,
                target_stable_id: row.get(1)?,
                relation: row.get(2)?,
                edge_kind: row.get(3)?,
                confidence: row.get(4)?,
                bind_method: row.get(5)?,
                direction: None,
                engine: KnowledgePathEngine::DuckPgq,
                status: KnowledgePathStatus::PathFound,
                caveat: None,
            })
        })
        .context("failed to run DuckPGQ direct path query")?;
    rows.enumerate()
        .map(|(path_index, row)| {
            let mut row = row.context("failed to read DuckPGQ direct path row")?;
            row.path_index = path_index;
            Ok(row)
        })
        .collect()
}

fn query_duckpgq_shortest_hops(
    conn: &duckdb::Connection,
    source_stable_id: &str,
    target_stable_id: &str,
    max_hops: usize,
) -> Result<Option<usize>> {
    load_analyst_duckpgq_extension(conn)?;
    let sql = duckpgq_shortest_hops_sql(source_stable_id, target_stable_id, max_hops);
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare DuckPGQ shortest path query")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .context("failed to run DuckPGQ shortest path query")?;
    let hops = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read DuckPGQ shortest path rows")?
        .into_iter()
        .next()
        .and_then(|hops| usize::try_from(hops).ok());
    Ok(hops)
}
