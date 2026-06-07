//! Shared Rust query layer over `.spur/analyst.duckdb`.

use std::path::Path;

use anyhow::{anyhow, Context as _, Result};

const MAX_CONTEXT_CANDIDATES: usize = 40;
const MAX_GRAPH_CANDIDATES: usize = 30;

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

#[derive(Debug, Clone)]
pub struct KnowledgeQueryOptions {
    pub limit: usize,
}

impl Default for KnowledgeQueryOptions {
    fn default() -> Self {
        Self { limit: 20 }
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

    let graph_content_hash = conn
        .query_row("SELECT graph_content_hash FROM _meta", [], |row| row.get(0))
        .ok();

    let escaped_query = query.replace('\'', "''");
    let sql_scope = scope.as_sql_scope();
    let sql = format!(
        "SELECT kind, title, file_path, stable_symbol_id, symbol_kind, score, \
         signal, neighbor_kind, edge_bind_method, grounding \
         FROM search_context_candidates('{escaped_query}', '{sql_scope}') \
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

    Ok(KnowledgeQueryResult {
        db_path: db_path.display().to_string(),
        graph_content_hash,
        candidates,
    })
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
    let sql = format!(
        "SELECT kind, title, file_path, stable_symbol_id, symbol_kind, score, \
         signal, neighbor_kind, edge_bind_method, grounding \
         FROM search_graph('{escaped_query}') \
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
