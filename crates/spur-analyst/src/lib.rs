//! Shared Rust query layer over `.spur/analyst.duckdb`.

use std::path::Path;

use anyhow::{anyhow, Context as _, Result};
use spur_graph::EMBEDDING_VECTOR_DIMENSIONS;

static LANCE_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

const MAX_CONTEXT_CANDIDATES: usize = 40;
const MAX_GRAPH_CANDIDATES: usize = 30;
pub const MAX_CONTEXT_PATH_HOPS: usize = 6;
pub const MAX_CONTEXT_PATHS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeSearchScope {
    All,
    Docs,
    Code,
    Graph,
}

impl KnowledgeSearchScope {
    fn as_sql_scope(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Docs => "docs",
            Self::Code => "code",
            Self::Graph => "graph",
        }
    }
}

impl TryFrom<&str> for KnowledgeSearchScope {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "docs" => Ok(Self::Docs),
            "code" => Ok(Self::Code),
            "graph" => Ok(Self::Graph),
            other => Err(anyhow!(
                "knowledge search scope must be one of all|docs|code|graph, got {other:?}"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeQueryIntent {
    Explain,
    Change,
    Review,
    Debug,
    Plan,
}

impl KnowledgeQueryIntent {
    fn as_sql_intent(self) -> &'static str {
        match self {
            Self::Explain => "explain",
            Self::Change => "change",
            Self::Review => "review",
            Self::Debug => "debug",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone)]
pub struct KnowledgeQueryOptions {
    pub limit: usize,
    pub intent: KnowledgeQueryIntent,
    pub query_vec: Option<Vec<f32>>,
}

impl Default for KnowledgeQueryOptions {
    fn default() -> Self {
        Self {
            limit: 20,
            intent: KnowledgeQueryIntent::Explain,
            query_vec: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeCandidate {
    pub kind: String,
    pub title: String,
    pub file_path: String,
    pub stable_symbol_id: Option<String>,
    pub symbol_kind: Option<String>,
    pub score: f64,
    pub signal: Option<String>,
    pub neighbor_kind: Option<String>,
    pub edge_bind_method: Option<String>,
    pub grounding: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeQueryResult {
    pub db_path: String,
    pub graph_content_hash: Option<String>,
    pub candidates: Vec<KnowledgeCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgePathEngine {
    DuckPgq,
    RecursiveSql,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgePathStatus {
    PathFound,
    NoPath,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnowledgePathOptions {
    pub max_hops: usize,
    pub max_paths: usize,
}

impl Default for KnowledgePathOptions {
    fn default() -> Self {
        Self {
            max_hops: 4,
            max_paths: 6,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgePathRow {
    pub path_index: usize,
    pub hop_index: usize,
    pub source_stable_id: String,
    pub target_stable_id: String,
    pub relation: Option<String>,
    pub edge_kind: Option<String>,
    pub confidence: Option<String>,
    pub bind_method: Option<String>,
    pub engine: KnowledgePathEngine,
    pub status: KnowledgePathStatus,
    pub caveat: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgePathResult {
    pub db_path: String,
    pub graph_content_hash: Option<String>,
    pub max_hops: usize,
    pub max_paths: usize,
    pub engine: KnowledgePathEngine,
    pub status: KnowledgePathStatus,
    pub caveat: Option<String>,
    pub rows: Vec<KnowledgePathRow>,
}

pub fn query_context_candidates(
    db_path: &Path,
    query: &str,
    scope: KnowledgeSearchScope,
    options: KnowledgeQueryOptions,
) -> Result<KnowledgeQueryResult> {
    let query = query.trim();
    if query.is_empty() {
        return Err(anyhow!("knowledge context query must be non-empty"));
    }
    let limit = options.limit.clamp(1, MAX_CONTEXT_CANDIDATES);

    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .context("failed to configure read-only duckdb")?;
    let conn = duckdb::Connection::open_with_flags(db_path, config).with_context(|| {
        format!(
            "failed to open analyst DuckDB read-only at {}",
            db_path.display()
        )
    })?;

    // The analyst scorecard can depend on TIMESTAMPTZ arithmetic whose overloads
    // live in DuckDB's ICU extension. Docs-only queries can still work without it,
    // so keep this best-effort and let query preparation surface real failures.
    let _ = conn.execute_batch("LOAD icu;");
    // Hybrid retrieval uses DuckDB's Lance extension when available. Keep this
    // best-effort so missing extension binaries degrade to the BM25 macro below.
    LANCE_INSTALLED.get_or_init(|| {
        let _ = conn.execute_batch("INSTALL lance;");
    });
    let _ = conn.execute_batch("LOAD lance;");

    let graph_content_hash = conn
        .query_row("SELECT graph_content_hash FROM _meta", [], |row| row.get(0))
        .ok();

    let escaped_query = query.replace('\'', "''");
    let sql_scope = scope.as_sql_scope();
    let sql_intent = options.intent.as_sql_intent();
    let query_vec_sql = format_query_vec_sql(options.query_vec.as_deref());
    let mut hybrid_failed = false;
    let candidates = match query_context_candidates_inner(
        &conn,
        &escaped_query,
        sql_scope,
        sql_intent,
        query_vec_sql.as_deref(),
        limit,
    ) {
        Ok(candidates) => candidates,
        Err(error) if query_vec_sql.is_some() => {
            hybrid_failed = true;
            tracing::warn!(
                error = %error,
                query,
                "hybrid search failed; degrading to BM25-only context candidate search"
            );
            query_context_candidates_inner(
                &conn,
                &escaped_query,
                sql_scope,
                sql_intent,
                None,
                limit,
            )?
        }
        Err(error) => return Err(error),
    };
    if query_vec_sql.is_some()
        && !hybrid_failed
        && candidates
            .iter()
            .all(|candidate| !candidate.grounding.starts_with("hybrid-"))
    {
        tracing::warn!(
            query,
            scope = sql_scope,
            intent = sql_intent,
            limit,
            "hybrid search produced no surviving hybrid-grounded context candidates"
        );
    }

    Ok(KnowledgeQueryResult {
        db_path: db_path.display().to_string(),
        graph_content_hash,
        candidates,
    })
}

pub fn query_context_paths(
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
    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .context("failed to configure read-only duckdb")?;
    let conn = duckdb::Connection::open_with_flags(db_path, config).with_context(|| {
        format!(
            "failed to open analyst DuckDB read-only at {}",
            db_path.display()
        )
    })?;

    let result_context = KnowledgePathResultContext {
        db_path,
        graph_content_hash: graph_content_hash(&conn),
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

    if let Ok(rows) =
        query_duckpgq_direct_paths(&conn, source_stable_id, target_stable_id, max_paths)
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

    match query_duckpgq_shortest_hops(&conn, source_stable_id, target_stable_id, max_hops) {
        Ok(Some(shortest_hops)) => {
            match query_recursive_context_path_rows(
                &conn,
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
        &conn,
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

struct KnowledgePathResultContext<'a> {
    db_path: &'a Path,
    graph_content_hash: Option<String>,
    max_hops: usize,
    max_paths: usize,
}

fn graph_content_hash(conn: &duckdb::Connection) -> Option<String> {
    conn.query_row("SELECT graph_content_hash FROM _meta", [], |row| row.get(0))
        .ok()
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
    conn.execute_batch("LOAD duckpgq;")
        .context("failed to load DuckPGQ extension")?;
    let source_sql = sql_string_literal(source_stable_id);
    let target_sql = sql_string_literal(target_stable_id);
    let sql = format!(
        "SELECT source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method \
         FROM GRAPH_TABLE (code \
           MATCH (a:duckpgq_nodes)-[e:duckpgq_edges]->(b:duckpgq_nodes) \
           WHERE a.stable_symbol_id = {source_sql} \
             AND b.stable_symbol_id = {target_sql} \
           COLUMNS (a.stable_symbol_id AS source_stable_id, \
                    b.stable_symbol_id AS target_stable_id, \
                    e.relation AS relation, \
                    e.edge_kind AS edge_kind, \
                    e.confidence AS confidence, \
                    e.bind_method AS bind_method)) \
         LIMIT {max_paths}"
    );
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
    conn.execute_batch("LOAD duckpgq;")
        .context("failed to load DuckPGQ extension")?;
    let source_sql = sql_string_literal(source_stable_id);
    let target_sql = sql_string_literal(target_stable_id);
    let sql = format!(
        "SELECT hops \
         FROM GRAPH_TABLE (code \
           MATCH p = ANY SHORTEST (a:duckpgq_nodes)-[e:duckpgq_edges]->{{1,{max_hops}}}(b:duckpgq_nodes) \
           WHERE a.stable_symbol_id = {source_sql} \
             AND b.stable_symbol_id = {target_sql} \
           COLUMNS (path_length(p) AS hops)) \
         LIMIT 1"
    );
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

fn query_recursive_context_path_rows(
    conn: &duckdb::Connection,
    source_stable_id: &str,
    target_stable_id: &str,
    max_hops: usize,
    max_paths: usize,
    engine: KnowledgePathEngine,
) -> Result<Vec<KnowledgePathRow>> {
    let sql = format!(
        "WITH RECURSIVE walk(current_id, depth, node_path, sort_key) AS ( \
           SELECT ?1::VARCHAR AS current_id, 0::INTEGER AS depth, [?1::VARCHAR] AS node_path, ?1::VARCHAR AS sort_key \
           UNION ALL \
           SELECT e.target_stable_id, w.depth + 1, list_append(w.node_path, e.target_stable_id), \
                  w.sort_key || '>' || e.target_stable_id \
           FROM walk w \
           JOIN edges e ON e.source_stable_id = w.current_id \
           WHERE w.depth < {max_hops} \
             AND e.target_stable_id IS NOT NULL \
             AND NOT list_contains(w.node_path, e.target_stable_id) \
         ), \
         complete_paths AS ( \
           SELECT row_number() OVER (ORDER BY depth, sort_key) - 1 AS path_index, depth, node_path \
           FROM walk \
           WHERE current_id = ?2 AND depth > 0 \
           ORDER BY depth, sort_key \
           LIMIT {max_paths} \
         ), \
         path_edges AS ( \
           SELECT path_index, idx - 1 AS hop_index, \
                  list_extract(node_path, idx) AS source_stable_id, \
                  list_extract(node_path, idx + 1) AS target_stable_id \
           FROM complete_paths \
           CROSS JOIN range(1, depth + 1) AS r(idx) \
         ), \
         ranked_edges AS ( \
           SELECT pe.path_index, pe.hop_index, e.source_stable_id, e.target_stable_id, \
                  e.relation, e.edge_kind, e.confidence, e.bind_method, \
                  row_number() OVER ( \
                    PARTITION BY pe.path_index, pe.hop_index \
                    ORDER BY e.relation, e.edge_kind, e.confidence, e.bind_method \
                  ) AS edge_rank \
           FROM path_edges pe \
           JOIN edges e \
             ON e.source_stable_id = pe.source_stable_id \
            AND e.target_stable_id = pe.target_stable_id \
         ) \
         SELECT path_index, hop_index, source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method \
         FROM ranked_edges \
         WHERE edge_rank = 1 \
         ORDER BY path_index, hop_index"
    );
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
                engine,
                status: KnowledgePathStatus::PathFound,
                caveat: None,
            })
        })
        .context("failed to run recursive context path query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read recursive context path rows")
}

fn i64_to_usize(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn query_context_candidates_inner(
    conn: &duckdb::Connection,
    escaped_query: &str,
    sql_scope: &str,
    sql_intent: &str,
    query_vec_sql: Option<&str>,
    limit: usize,
) -> Result<Vec<KnowledgeCandidate>> {
    let macro_call = match query_vec_sql {
        Some(query_vec_sql) => format!(
            "search_context_candidates_hybrid('{escaped_query}', '{sql_scope}', '{sql_intent}', {query_vec_sql})"
        ),
        None => format!("search_context_candidates('{escaped_query}', '{sql_scope}', '{sql_intent}')"),
    };
    let sql = format!(
        "SELECT kind, title, file_path, stable_symbol_id, symbol_kind, score, \
         signal, neighbor_kind, edge_bind_method, grounding \
         FROM {macro_call} \
         LIMIT {limit}"
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare context candidate query")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(KnowledgeCandidate {
                kind: row.get(0)?,
                title: row.get(1)?,
                file_path: row.get(2)?,
                stable_symbol_id: row.get(3)?,
                symbol_kind: row.get(4)?,
                score: row.get(5)?,
                signal: row.get(6)?,
                neighbor_kind: row.get(7)?,
                edge_bind_method: row.get(8)?,
                grounding: row.get(9)?,
            })
        })
        .context("failed to run context candidate query")?;
    let candidates = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read context candidate rows")?;

    Ok(candidates)
}

fn format_query_vec_sql(query_vec: Option<&[f32]>) -> Option<String> {
    let query_vec = query_vec?;
    if query_vec.len() != EMBEDDING_VECTOR_DIMENSIONS
        || query_vec.iter().any(|value| !value.is_finite())
    {
        return None;
    }

    let mut sql = String::from("[");
    for (index, value) in query_vec.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&value.to_string());
    }
    sql.push_str("]::FLOAT[");
    sql.push_str(&EMBEDDING_VECTOR_DIMENSIONS.to_string());
    sql.push(']');
    Some(sql)
}

pub fn query_graph_candidates(
    db_path: &Path,
    query: &str,
    options: KnowledgeQueryOptions,
) -> Result<KnowledgeQueryResult> {
    let query = query.trim();
    if query.is_empty() {
        return Err(anyhow!("knowledge graph query must be non-empty"));
    }
    let limit = options.limit.clamp(1, MAX_GRAPH_CANDIDATES);

    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .context("failed to configure read-only duckdb")?;
    let conn = duckdb::Connection::open_with_flags(db_path, config).with_context(|| {
        format!(
            "failed to open analyst DuckDB read-only at {}",
            db_path.display()
        )
    })?;

    // The analyst scorecard can depend on TIMESTAMPTZ arithmetic whose overloads
    // live in DuckDB's ICU extension. Docs-only queries can still work without it,
    // so keep this best-effort and let query preparation surface real failures.
    let _ = conn.execute_batch("LOAD icu;");

    let graph_content_hash = conn
        .query_row("SELECT graph_content_hash FROM _meta", [], |row| row.get(0))
        .ok();

    let escaped_query = query.replace('\'', "''");
    let sql_intent = options.intent.as_sql_intent();
    let sql = format!(
        "SELECT kind, title, file_path, stable_symbol_id, symbol_kind, score, \
         signal, neighbor_kind, edge_bind_method, grounding \
         FROM search_graph('{escaped_query}', '{sql_intent}') \
         LIMIT {limit}"
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare graph candidate query")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(KnowledgeCandidate {
                kind: row.get(0)?,
                title: row.get(1)?,
                file_path: row.get(2)?,
                stable_symbol_id: row.get(3)?,
                symbol_kind: row.get(4)?,
                score: row.get(5)?,
                signal: row.get(6)?,
                neighbor_kind: row.get(7)?,
                edge_bind_method: row.get(8)?,
                grounding: row.get(9)?,
            })
        })
        .context("failed to run graph candidate query")?;
    let candidates = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read graph candidate rows")?;

    Ok(KnowledgeQueryResult {
        db_path: db_path.display().to_string(),
        graph_content_hash,
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::format_query_vec_sql;
    use spur_graph::EMBEDDING_VECTOR_DIMENSIONS;

    #[test]
    fn format_query_vec_sql_rejects_wrong_dimension() {
        assert!(format_query_vec_sql(Some(&vec![0.0; EMBEDDING_VECTOR_DIMENSIONS - 1])).is_none());
        assert!(format_query_vec_sql(Some(&vec![0.0; EMBEDDING_VECTOR_DIMENSIONS + 1])).is_none());
        assert!(format_query_vec_sql(Some(&vec![0.0; EMBEDDING_VECTOR_DIMENSIONS])).is_some());
    }
}
