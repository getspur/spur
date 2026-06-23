//! Shared Rust query layer over `.spur/analyst.duckdb`.

pub mod mcp;

use std::path::Path;

use anyhow::{anyhow, Context as _, Result};
use spur_graph::EMBEDDING_VECTOR_DIMENSIONS;

static LANCE_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
// duckpgq is distributed via DuckDB's community repo (not the core repo) and is
// only published for DuckDB <= 1.4.4. Install it once per process; the LOAD is
// best-effort and the caller falls back to recursive SQL on failure.
static DUCKPGQ_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

const MAX_CONTEXT_CANDIDATES: usize = 40;
const MAX_GRAPH_CANDIDATES: usize = 30;
pub const MAX_SYMBOL_RISK_COMMUNITY_IDS: usize = 40;
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
pub enum SymbolEvidenceStatus {
    Available,
    MissingSymbol,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolEvidenceCaveat {
    pub stable_symbol_id: Option<String>,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolRiskScorecardRow {
    pub input_index: usize,
    pub stable_symbol_id: String,
    pub status: SymbolEvidenceStatus,
    pub entity_name: Option<String>,
    pub qualified_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub file_path: Option<String>,
    pub pagerank: Option<f64>,
    pub in_degree: Option<i64>,
    pub out_degree: Option<i64>,
    pub callers: Option<i64>,
    pub importers: Option<i64>,
    pub inbound_total: Option<i64>,
    pub churn_90d: Option<i64>,
    pub last_touched: Option<String>,
    pub blast_radius_score: Option<f64>,
    pub posture: Option<String>,
    pub caveats: Vec<SymbolEvidenceCaveat>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolCommunityContextRow {
    pub input_index: usize,
    pub stable_symbol_id: String,
    pub status: SymbolEvidenceStatus,
    pub component_id: Option<i64>,
    pub component_size: Option<i64>,
    pub community_id: Option<i64>,
    pub caveats: Vec<SymbolEvidenceCaveat>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolGraphMetrics {
    pub calls_edges: Option<i64>,
    pub connected_nodes: Option<i64>,
    pub components: Option<i64>,
    pub largest_component: Option<i64>,
    pub communities: Option<i64>,
    pub density: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolRiskCommunityResult {
    pub db_path: String,
    pub graph_content_hash: Option<String>,
    pub max_symbols: usize,
    pub truncated: bool,
    pub risk_scorecard: Vec<SymbolRiskScorecardRow>,
    pub community_context: Vec<SymbolCommunityContextRow>,
    pub graph_metrics: Option<SymbolGraphMetrics>,
    pub caveats: Vec<SymbolEvidenceCaveat>,
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
    pub undirected: bool,
}

impl Default for KnowledgePathOptions {
    fn default() -> Self {
        Self {
            max_hops: 4,
            max_paths: 6,
            undirected: false,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
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

pub fn query_symbol_risk_community<S: AsRef<str>>(
    db_path: &Path,
    stable_symbol_ids: &[S],
) -> Result<SymbolRiskCommunityResult> {
    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .context("failed to configure read-only duckdb")?;
    let conn = duckdb::Connection::open_with_flags(db_path, config).with_context(|| {
        format!(
            "failed to open analyst DuckDB read-only at {}",
            db_path.display()
        )
    })?;

    // The risk scorecard view (via v_symbol_churn_90d) does TIMESTAMPTZ arithmetic
    // (`now() - INTERVAL '90 day'`) whose `-` overload lives in DuckDB's ICU
    // extension. Without it, scorecard enrichment fails to bind. Keep it
    // best-effort (mirrors query_context_candidates) and let preparation surface
    // any genuine failure as a caveat.
    let _ = conn.execute_batch("INSTALL icu; LOAD icu;");

    let (inputs, mut caveats, truncated) = bounded_symbol_inputs(stable_symbol_ids);
    let mut result = SymbolRiskCommunityResult {
        db_path: db_path.display().to_string(),
        graph_content_hash: graph_content_hash(&conn),
        max_symbols: MAX_SYMBOL_RISK_COMMUNITY_IDS,
        truncated,
        risk_scorecard: Vec::new(),
        community_context: Vec::new(),
        graph_metrics: None,
        caveats: Vec::new(),
    };
    result.caveats.append(&mut caveats);

    if inputs.is_empty() {
        return Ok(result);
    }

    match query_symbol_risk_scorecard_rows(&conn, &inputs) {
        Ok(rows) => result.risk_scorecard = rows,
        Err(error) => {
            let caveat = SymbolEvidenceCaveat {
                stable_symbol_id: None,
                code: "scorecard_unavailable".to_owned(),
                message: format!("v_symbol_scorecard unavailable: {error:#}"),
            };
            result
                .risk_scorecard
                .extend(unavailable_risk_rows(&inputs, &caveat));
            result.caveats.push(caveat);
        }
    }

    match query_symbol_community_context_rows(&conn, &inputs) {
        Ok(rows) => result.community_context = rows,
        Err(error) => {
            let caveat = SymbolEvidenceCaveat {
                stable_symbol_id: None,
                code: "community_unavailable".to_owned(),
                message: format!("v_symbol_component/v_symbol_community unavailable: {error:#}"),
            };
            result
                .community_context
                .extend(unavailable_community_rows(&inputs, &caveat));
            result.caveats.push(caveat);
        }
    }

    match query_symbol_graph_metrics(&conn) {
        Ok(metrics) => result.graph_metrics = metrics,
        Err(error) => result.caveats.push(SymbolEvidenceCaveat {
            stable_symbol_id: None,
            code: "graph_metrics_unavailable".to_owned(),
            message: format!("v_graph_metrics unavailable: {error:#}"),
        }),
    }

    Ok(result)
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
    let _ = conn.execute_batch("INSTALL icu; LOAD icu;");
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

    if options.undirected {
        match query_recursive_undirected_context_path_rows(
            &conn,
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
    DUCKPGQ_INSTALLED.get_or_init(|| {
        let _ = conn.execute_batch("INSTALL duckpgq FROM community;");
    });
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
    DUCKPGQ_INSTALLED.get_or_init(|| {
        let _ = conn.execute_batch("INSTALL duckpgq FROM community;");
    });
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
        "WITH RECURSIVE traversable_edges AS ( \
           SELECT source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method \
           FROM edges \
           WHERE relation = 'calls' \
             AND edge_kind IN ('calls', 'calls_dyn', 'references_hof') \
         ), \
         walk(current_id, depth, node_path, sort_key) AS ( \
           SELECT ?1::VARCHAR AS current_id, 0::INTEGER AS depth, [?1::VARCHAR] AS node_path, ?1::VARCHAR AS sort_key \
           UNION ALL \
           SELECT e.target_stable_id, w.depth + 1, list_append(w.node_path, e.target_stable_id), \
                  w.sort_key || '>' || e.target_stable_id \
           FROM walk w \
           JOIN traversable_edges e ON e.source_stable_id = w.current_id \
           WHERE w.depth < {max_hops} \
             AND e.target_stable_id IS NOT NULL \
             AND NOT list_contains(w.node_path, e.target_stable_id) \
         ), \
         complete_paths AS ( \
           SELECT row_number() OVER (ORDER BY depth, sort_key) - 1 AS path_index, depth, node_path \
           FROM ( \
             SELECT DISTINCT depth, node_path, sort_key \
             FROM walk \
             WHERE current_id = ?2 AND depth > 0 \
           ) \
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
           JOIN traversable_edges e \
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
    let sql = format!(
        "WITH RECURSIVE traversable_edges AS ( \
            SELECT source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method \
            FROM edges \
            WHERE relation = 'calls' \
              AND edge_kind IN ('calls', 'calls_dyn', 'references_hof') \
         ), \
         edges_undirected AS ( \
            SELECT source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method, 'forward' AS direction FROM traversable_edges \
            UNION ALL \
            SELECT target_stable_id AS source_stable_id, source_stable_id AS target_stable_id, relation, edge_kind, confidence, bind_method, 'reverse' AS direction FROM traversable_edges \
         ), \
         walk(current_id, depth, node_path, sort_key) AS ( \
            SELECT ?1::VARCHAR AS current_id, 0::INTEGER AS depth, [?1::VARCHAR] AS node_path, ?1::VARCHAR AS sort_key \
            UNION ALL \
            SELECT e.target_stable_id, w.depth + 1, list_append(w.node_path, e.target_stable_id), \
                   w.sort_key || '>' || e.target_stable_id \
            FROM walk w \
            JOIN edges_undirected e ON e.source_stable_id = w.current_id \
            WHERE w.depth < {max_hops} \
              AND e.target_stable_id IS NOT NULL \
              AND NOT list_contains(w.node_path, e.target_stable_id) \
          ), \
          complete_paths AS ( \
            SELECT row_number() OVER (ORDER BY depth, sort_key) - 1 AS path_index, depth, node_path \
            FROM ( \
              SELECT DISTINCT depth, node_path, sort_key \
              FROM walk \
              WHERE current_id = ?2 AND depth > 0 \
            ) \
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
                   e.relation, e.edge_kind, e.confidence, e.bind_method, e.direction, \
                   row_number() OVER ( \
                     PARTITION BY pe.path_index, pe.hop_index \
                     ORDER BY e.relation, e.edge_kind, e.confidence, e.bind_method \
                   ) AS edge_rank \
            FROM path_edges pe \
            JOIN edges_undirected e \
              ON e.source_stable_id = pe.source_stable_id \
             AND e.target_stable_id = pe.target_stable_id \
          ) \
          SELECT path_index, hop_index, source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method, direction \
          FROM ranked_edges \
          WHERE edge_rank = 1 \
          ORDER BY path_index, hop_index"
    );
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

fn i64_to_usize(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[derive(Debug, Clone)]
struct SymbolInput {
    input_index: usize,
    stable_symbol_id: String,
}

fn bounded_symbol_inputs<S: AsRef<str>>(
    stable_symbol_ids: &[S],
) -> (Vec<SymbolInput>, Vec<SymbolEvidenceCaveat>, bool) {
    let mut inputs = Vec::new();
    let mut caveats = Vec::new();
    let mut truncated = false;

    for (input_index, stable_symbol_id) in stable_symbol_ids.iter().enumerate() {
        let stable_symbol_id = stable_symbol_id.as_ref().trim();
        if stable_symbol_id.is_empty() {
            caveats.push(SymbolEvidenceCaveat {
                stable_symbol_id: None,
                code: "empty_stable_symbol_id".to_owned(),
                message: format!("ignored empty stable symbol ID at input index {input_index}"),
            });
            continue;
        }
        if inputs.len() >= MAX_SYMBOL_RISK_COMMUNITY_IDS {
            truncated = true;
            continue;
        }
        inputs.push(SymbolInput {
            input_index,
            stable_symbol_id: stable_symbol_id.to_owned(),
        });
    }

    if truncated {
        caveats.push(SymbolEvidenceCaveat {
            stable_symbol_id: None,
            code: "input_truncated".to_owned(),
            message: format!(
                "symbol enrichment input was capped at {MAX_SYMBOL_RISK_COMMUNITY_IDS} stable IDs"
            ),
        });
    }

    (inputs, caveats, truncated)
}

fn query_symbol_risk_scorecard_rows(
    conn: &duckdb::Connection,
    inputs: &[SymbolInput],
) -> Result<Vec<SymbolRiskScorecardRow>> {
    let input_values = symbol_input_values_sql(inputs);
    let sql = format!(
        "WITH input(input_index, stable_symbol_id) AS (VALUES {input_values}) \
         SELECT i.input_index, i.stable_symbol_id, sc.stable_symbol_id, \
                sc.entity_name, sc.qualified_name, sc.symbol_kind, sc.file_path, \
                sc.pagerank, sc.in_degree, sc.out_degree, sc.callers, sc.importers, \
                sc.inbound_total, sc.churn_90d, CAST(sc.last_touched AS VARCHAR), \
                sc.blast_radius_score, sc.posture \
         FROM input i \
         LEFT JOIN v_symbol_scorecard sc \
           ON sc.stable_symbol_id = i.stable_symbol_id \
         ORDER BY i.input_index"
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare symbol scorecard enrichment query")?;
    let rows = stmt
        .query_map([], |row| {
            let input_index = i64_to_usize(row.get(0)?);
            let stable_symbol_id: String = row.get(1)?;
            let matched_symbol_id: Option<String> = row.get(2)?;
            let status = if matched_symbol_id.is_some() {
                SymbolEvidenceStatus::Available
            } else {
                SymbolEvidenceStatus::MissingSymbol
            };
            let caveats = if status == SymbolEvidenceStatus::MissingSymbol {
                vec![SymbolEvidenceCaveat {
                    stable_symbol_id: Some(stable_symbol_id.clone()),
                    code: "scorecard_missing_symbol".to_owned(),
                    message: format!(
                        "stable symbol ID {stable_symbol_id:?} has no row in v_symbol_scorecard"
                    ),
                }]
            } else {
                Vec::new()
            };
            Ok(SymbolRiskScorecardRow {
                input_index,
                stable_symbol_id,
                status,
                entity_name: row.get(3)?,
                qualified_name: row.get(4)?,
                symbol_kind: row.get(5)?,
                file_path: row.get(6)?,
                pagerank: row.get(7)?,
                in_degree: row.get(8)?,
                out_degree: row.get(9)?,
                callers: row.get(10)?,
                importers: row.get(11)?,
                inbound_total: row.get(12)?,
                churn_90d: row.get(13)?,
                last_touched: row.get(14)?,
                blast_radius_score: row.get(15)?,
                posture: row.get(16)?,
                caveats,
            })
        })
        .context("failed to run symbol scorecard enrichment query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read symbol scorecard enrichment rows")
}

fn query_symbol_community_context_rows(
    conn: &duckdb::Connection,
    inputs: &[SymbolInput],
) -> Result<Vec<SymbolCommunityContextRow>> {
    let input_values = symbol_input_values_sql(inputs);
    let sql = format!(
        "WITH input(input_index, stable_symbol_id) AS (VALUES {input_values}) \
         SELECT i.input_index, i.stable_symbol_id, \
                cmp.stable_symbol_id, comm.stable_symbol_id, \
                cmp.component_id, cmp.component_size, comm.community_id \
         FROM input i \
         LEFT JOIN v_symbol_component cmp \
           ON cmp.stable_symbol_id = i.stable_symbol_id \
         LEFT JOIN v_symbol_community comm \
           ON comm.stable_symbol_id = i.stable_symbol_id \
         ORDER BY i.input_index"
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare symbol community enrichment query")?;
    let rows = stmt
        .query_map([], |row| {
            let input_index = i64_to_usize(row.get(0)?);
            let stable_symbol_id: String = row.get(1)?;
            let component_symbol_id: Option<String> = row.get(2)?;
            let community_symbol_id: Option<String> = row.get(3)?;
            let status = if component_symbol_id.is_some() || community_symbol_id.is_some() {
                SymbolEvidenceStatus::Available
            } else {
                SymbolEvidenceStatus::MissingSymbol
            };
            let caveats = if status == SymbolEvidenceStatus::MissingSymbol {
                vec![SymbolEvidenceCaveat {
                    stable_symbol_id: Some(stable_symbol_id.clone()),
                    code: "community_missing_symbol".to_owned(),
                    message: format!(
                        "stable symbol ID {stable_symbol_id:?} has no row in v_symbol_component or v_symbol_community"
                    ),
                }]
            } else {
                Vec::new()
            };
            Ok(SymbolCommunityContextRow {
                input_index,
                stable_symbol_id,
                status,
                component_id: row.get(4)?,
                component_size: row.get(5)?,
                community_id: row.get(6)?,
                caveats,
            })
        })
        .context("failed to run symbol community enrichment query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read symbol community enrichment rows")
}

fn query_symbol_graph_metrics(conn: &duckdb::Connection) -> Result<Option<SymbolGraphMetrics>> {
    let mut stmt = conn
        .prepare(
            "SELECT calls_edges, connected_nodes, components, largest_component, communities, density \
             FROM v_graph_metrics \
             LIMIT 1",
        )
        .context("failed to prepare graph metrics query")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SymbolGraphMetrics {
                calls_edges: row.get(0)?,
                connected_nodes: row.get(1)?,
                components: row.get(2)?,
                largest_component: row.get(3)?,
                communities: row.get(4)?,
                density: row.get(5)?,
            })
        })
        .context("failed to run graph metrics query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read graph metrics rows")
        .map(|rows| rows.into_iter().next())
}

fn unavailable_risk_rows(
    inputs: &[SymbolInput],
    caveat: &SymbolEvidenceCaveat,
) -> Vec<SymbolRiskScorecardRow> {
    inputs
        .iter()
        .map(|input| SymbolRiskScorecardRow {
            input_index: input.input_index,
            stable_symbol_id: input.stable_symbol_id.clone(),
            status: SymbolEvidenceStatus::Unavailable,
            entity_name: None,
            qualified_name: None,
            symbol_kind: None,
            file_path: None,
            pagerank: None,
            in_degree: None,
            out_degree: None,
            callers: None,
            importers: None,
            inbound_total: None,
            churn_90d: None,
            last_touched: None,
            blast_radius_score: None,
            posture: None,
            caveats: vec![SymbolEvidenceCaveat {
                stable_symbol_id: Some(input.stable_symbol_id.clone()),
                code: caveat.code.clone(),
                message: caveat.message.clone(),
            }],
        })
        .collect()
}

fn unavailable_community_rows(
    inputs: &[SymbolInput],
    caveat: &SymbolEvidenceCaveat,
) -> Vec<SymbolCommunityContextRow> {
    inputs
        .iter()
        .map(|input| SymbolCommunityContextRow {
            input_index: input.input_index,
            stable_symbol_id: input.stable_symbol_id.clone(),
            status: SymbolEvidenceStatus::Unavailable,
            component_id: None,
            component_size: None,
            community_id: None,
            caveats: vec![SymbolEvidenceCaveat {
                stable_symbol_id: Some(input.stable_symbol_id.clone()),
                code: caveat.code.clone(),
                message: caveat.message.clone(),
            }],
        })
        .collect()
}

fn symbol_input_values_sql(inputs: &[SymbolInput]) -> String {
    inputs
        .iter()
        .map(|input| {
            format!(
                "({}, {})",
                input.input_index,
                sql_string_literal(&input.stable_symbol_id)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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
    let _ = conn.execute_batch("INSTALL icu; LOAD icu;");

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
    use std::path::PathBuf;

    use duckdb::Connection;
    use spur_graph::EMBEDDING_VECTOR_DIMENSIONS;

    use super::*;

    #[test]
    fn format_query_vec_sql_rejects_wrong_dimension() {
        assert!(format_query_vec_sql(Some(&vec![0.0; EMBEDDING_VECTOR_DIMENSIONS - 1])).is_none());
        assert!(format_query_vec_sql(Some(&vec![0.0; EMBEDDING_VECTOR_DIMENSIONS + 1])).is_none());
        assert!(format_query_vec_sql(Some(&vec![0.0; EMBEDDING_VECTOR_DIMENSIONS])).is_some());
    }

    fn context_path_fixture(edges_sql: &str) -> (tempfile::TempDir, PathBuf) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("analyst.duckdb");
        let conn = Connection::open(&db_path).expect("open fixture db");
        conn.execute_batch(
            r#"
            CREATE TABLE _meta (graph_content_hash VARCHAR);
            INSERT INTO _meta VALUES ('fixture-hash');

            CREATE TABLE edges (
                source_stable_id VARCHAR,
                target_stable_id VARCHAR,
                relation VARCHAR,
                edge_kind VARCHAR,
                confidence VARCHAR,
                bind_method VARCHAR
            );
            "#,
        )
        .expect("create path fixture schema");
        conn.execute_batch(edges_sql)
            .expect("insert path fixture edges");
        drop(conn);
        (temp_dir, db_path)
    }

    #[test]
    fn query_context_paths_includes_calls_dyn_edges() {
        let (_temp_dir, db_path) = context_path_fixture(
            r#"
            INSERT INTO edges VALUES
                ('sym-source', 'sym-target', 'calls', 'calls_dyn', 'dynamic_dispatch', 'label_match');
            "#,
        );

        let result = query_context_paths(
            &db_path,
            "sym-source",
            "sym-target",
            KnowledgePathOptions {
                max_hops: 1,
                max_paths: 4,
                undirected: false,
            },
        )
        .expect("query context paths");

        assert_eq!(result.status, KnowledgePathStatus::PathFound);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].relation.as_deref(), Some("calls"));
        assert_eq!(result.rows[0].edge_kind.as_deref(), Some("calls_dyn"));
    }

    #[test]
    fn query_context_paths_includes_hof_edges_and_dedupes_sequences() {
        let (_temp_dir, db_path) = context_path_fixture(
            r#"
            INSERT INTO edges VALUES
                ('sym-source', 'sym-dyn', 'calls', 'calls_dyn', 'dynamic_dispatch', 'label_match'),
                ('sym-source', 'sym-dyn', 'calls', 'calls_dyn', 'dynamic_dispatch', 'label_match'),
                ('sym-dyn', 'sym-target', 'calls', 'references_hof', 'higher_order', 'label_match'),
                ('sym-dyn', 'sym-target', 'calls', 'references_hof', 'higher_order', 'label_match'),
                ('sym-source', 'sym-module', 'contains', 'references_other', 'syntax_exact', 'scope'),
                ('sym-module', 'sym-target', 'imports', 'references_other', 'syntax_exact', 'external');
            "#,
        );

        let result = query_context_paths(
            &db_path,
            "sym-source",
            "sym-target",
            KnowledgePathOptions {
                max_hops: 2,
                max_paths: 4,
                undirected: true,
            },
        )
        .expect("query context paths");

        assert_eq!(result.status, KnowledgePathStatus::PathFound);
        assert!(
            result
                .rows
                .iter()
                .all(|row| row.relation.as_deref() == Some("calls")
                    && matches!(
                        row.edge_kind.as_deref(),
                        Some("calls_dyn" | "references_hof")
                    )),
            "path rows must exclude containment/import edges: {:?}",
            result.rows
        );
        assert!(
            result
                .rows
                .iter()
                .any(|row| row.edge_kind.as_deref() == Some("calls_dyn")),
            "dynamic-dispatch call edge should be visible: {:?}",
            result.rows
        );
        assert!(
            result
                .rows
                .iter()
                .any(|row| row.edge_kind.as_deref() == Some("references_hof")),
            "higher-order call edge should be visible: {:?}",
            result.rows
        );

        let mut rows_by_path = std::collections::BTreeMap::new();
        for row in &result.rows {
            rows_by_path
                .entry(row.path_index)
                .or_insert_with(Vec::new)
                .push(row);
        }
        let mut path_sequences = rows_by_path
            .values()
            .map(|rows| {
                let mut sequence = rows
                    .iter()
                    .map(|row| row.source_stable_id.as_str())
                    .collect::<Vec<_>>();
                sequence.push(
                    rows.last()
                        .expect("path rows should not be empty")
                        .target_stable_id
                        .as_str(),
                );
                sequence
            })
            .collect::<Vec<_>>();
        path_sequences.sort_unstable();
        path_sequences.dedup();

        assert_eq!(
            path_sequences.len(),
            rows_by_path.len(),
            "duplicate full node-sequences must be collapsed before max_paths cap"
        );
        assert_eq!(
            path_sequences,
            vec![vec!["sym-source", "sym-dyn", "sym-target"]]
        );
    }
}
